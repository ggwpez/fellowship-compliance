use core::{iter, time::Duration};
use parity_scale_codec as Codec;
use parity_scale_codec::Encode;
use sp_core::{
	crypto::{AccountId32, Ss58Codec},
	ByteArray,
};
use std::{
	collections::BTreeMap,
	io::{Read, Write},
	num::NonZeroU32,
	time::{SystemTime, UNIX_EPOCH},
};
use subxt::{
	client::{LightClient, RawLightClient},
	PolkadotConfig, *,
};

#[subxt::subxt(runtime_metadata_path = "metadata/polkadot.scale")]
pub mod polkadot {}

#[subxt::subxt(runtime_metadata_path = "metadata/collectives-polkadot.scale")]
pub mod collectives {}

pub type Registration = polkadot::runtime_types::pallet_identity::types::Registration<
	u128,
	polkadot::runtime_types::pallet_identity::simple::IdentityInfo,
>;

const RPC_COOLDOWN_MS: u64 = 2000;

#[derive(Debug, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub struct Fellow {
	pub account: AccountId32,
	pub rank: u16,
	pub identity: Option<Registration>,
	pub github: Option<String>,
	pub github_links_back: bool,
	pub score: Option<u32>, // [0, 1]
}

fn data_to_str(data: &polkadot::runtime_types::pallet_identity::types::Data) -> Option<String> {
	// Wtf...
	let mut encoded = data.encode();
	if encoded[0] >= 1 && encoded[0] <= 33 {
		encoded.remove(0);
		let raw = String::from_utf8(encoded).ok()?;

		return Some(raw);
	}
	None
}

impl Fellow {
	pub fn address(&self) -> String {
		self.account
			.to_ss58check_with_version(sp_core::crypto::Ss58AddressFormat::custom(0))
	}

	pub fn name(&self) -> Option<String> {
		self.identity.as_ref().and_then(|i| data_to_str(&i.info.display))
	}

	pub fn verified(&self) -> bool {
		use polkadot::runtime_types::pallet_identity::types::Judgement::*;
		self.identity
			.as_ref()
			.map(|r| {
				for (_, j) in r.judgements.0.iter() {
					if matches!(j, Reasonable | KnownGood) {
						return true;
					}
				}
				false
			})
			.unwrap_or(false)
	}

	pub fn github_verified(&self) -> bool {
		self.github_links_back
	}

	fn reg_to_github(r: &Registration) -> Option<String> {
		for (k, v) in r.info.additional.0.iter() {
			if data_to_str(k) == Some("github".to_string()) {
				return data_to_str(v);
			}
		}
		None
	}

	pub fn github(&self) -> Option<String> {
		self.identity.as_ref().and_then(Self::reg_to_github)
	}
}

#[derive(Default, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub struct Fellows {
	pub members: BTreeMap<AccountId32, Fellow>,
	pub last_updated: Option<u64>,
	#[codec(skip)]
	pub num_named: u32,
	#[codec(skip)]
	pub num_verified: u32,
	#[codec(skip)]
	pub num_github: u32,
	#[codec(skip)]
	pub num_github_verified: u32,
	#[codec(skip)]
	pub num_accounts: u32,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type Client = OnlineClient<subxt::SubstrateConfig>;

impl Fellows {
	fn now() -> Self {
		Self { last_updated: Some(now_s()), ..Default::default() }
	}

	pub fn since_last_update(&self) -> Option<Duration> {
		Some(Duration::from_secs(now_s() - self.last_updated?))
	}

	pub async fn load() -> Result<Self> {
		let path = std::path::Path::new("data.scale");
		if path.exists() {
			log::info!("Loading from cache...");

			match Self::try_from_cache() {
				Ok(s) => {
					log::info!("Loaded from cache");
					return Ok(Self::finalize(s));
				},
				Err(e) => log::warn!("Failed to load from cache. Falling back to fetch: {}", e),
			}
		} else {
			log::info!("Path {} does not exist. Falling back to fetch", path.display());
		}

		Self::fetch().await
	}

	fn try_from_cache() -> Result<Self> {
		let mut file = std::fs::File::open("data.scale")?;
		let mut data = Vec::new();
		file.read_to_end(&mut data)?;
		parity_scale_codec::Decode::decode(&mut &data[..]).map_err(Into::into)
	}

	fn finalize(mut self) -> Self {
		for (_, member) in self.members.iter() {
			self.num_accounts += 1;

			if member.verified() {
				self.num_verified += 1;
			}

			if member.github().is_some() {
				self.num_github += 1;
			}

			if member.github_verified() {
				self.num_github_verified += 1;
			}

			if member.name().is_some() {
				self.num_named += 1;
			}
		}

		self
	}

	pub async fn fetch() -> Result<Self> {
		let timeout = Duration::from_secs(600); // 10 min timeout for syncing and fetching
		tokio::time::timeout(timeout, Self::do_fetch()).await?
	}

	async fn do_fetch() -> Result<Self> {
		let mut s = Self::now();
		log::info!("Fetching data...");

		let (polkadot_api, collectives_api) = Self::build_clients().await?;

		s.fetch_fellows(&collectives_api).await?;
		s.fetch_identities(&polkadot_api).await?;
		std::mem::drop((polkadot_api, collectives_api));
		s.fetch_github().await?;

		// Store in a file for faster restarts if it crashed
		let mut file = std::fs::File::create("data.scale")?;
		let data = parity_scale_codec::Encode::encode(&s);
		file.write_all(&data)?;
		log::info!("Data written to data.scale");

		Ok(s.finalize())
	}

	/// Build a light client for the relay chain and the parachain.
	async fn build_clients() -> Result<(LightClient<PolkadotConfig>, LightClient<PolkadotConfig>)> {
		let mut client = subxt_lightclient::smoldot::Client::new(
			subxt_lightclient::smoldot::DefaultPlatform::new(
				"subxt-example-light-client".into(),
				"version-0".into(),
			),
		);
		log::info!("Setting up light clients #1");

		let polkadot_connection = client
			.add_chain(subxt_lightclient::smoldot::AddChainConfig {
				specification: include_str!("../metadata/polkadot.json"),
				json_rpc: subxt_lightclient::smoldot::AddChainConfigJsonRpc::Enabled {
					max_pending_requests: NonZeroU32::new(128).unwrap(),
					max_subscriptions: 1024,
				},
				potential_relay_chains: iter::empty(),
				database_content: "",
				user_data: (),
			})
			.expect("Light client chain added with valid spec; qed");
		let polkadot_json_rpc_responses = polkadot_connection
			.json_rpc_responses
			.expect("Light client configured with json rpc enabled; qed");
		let polkadot_chain_id = polkadot_connection.chain_id;
		log::info!("Setting up light clients #2");

		// Step 3. Connect to the parachain. For this example, the Asset hub parachain.
		let assethub_connection = client
			.add_chain(subxt_lightclient::smoldot::AddChainConfig {
				specification: include_str!("../metadata/collectives-polkadot.json"),
				json_rpc: subxt_lightclient::smoldot::AddChainConfigJsonRpc::Enabled {
					max_pending_requests: NonZeroU32::new(128).unwrap(),
					max_subscriptions: 1024,
				},
				// The chain specification of the asset hub parachain mentions that the identifier
				// of its relay chain is `polkadot`.
				potential_relay_chains: [polkadot_chain_id].into_iter(),
				database_content: "",
				user_data: (),
			})
			.expect("Light client chain added with valid spec; qed");
		let parachain_json_rpc_responses = assethub_connection
			.json_rpc_responses
			.expect("Light client configured with json rpc enabled; qed");
		let parachain_chain_id = assethub_connection.chain_id;

		// Step 4. Turn the smoldot client into a raw client.
		let raw_light_client = RawLightClient::builder()
			.add_chain(polkadot_chain_id, polkadot_json_rpc_responses)
			.add_chain(parachain_chain_id, parachain_json_rpc_responses)
			.build(client)
			.await?;

		// Step 5. Obtain a client to target the relay chain and the parachain.
		let polkadot_api: LightClient<PolkadotConfig> =
			raw_light_client.for_chain(polkadot_chain_id).await?;
		let parachain_api: LightClient<PolkadotConfig> =
			raw_light_client.for_chain(parachain_chain_id).await?;
		log::info!("Connected to light clients");

		// Step 6. Subscribe to the finalized blocks of the chains.

		Ok((polkadot_api, parachain_api))
	}

	async fn fetch_fellows(&mut self, collectives_rpc: &LightClient<PolkadotConfig>) -> Result<()> {
		log::info!("Fetching collectives data...");
		let mut members = BTreeMap::new();
		let key = collectives::storage().fellowship_collective().members_iter();
		let mut query = collectives_rpc.storage().at_latest().await.unwrap().iter(key).await?;

		while let Some(Ok((id, fellow))) = query.next().await {
			let account = AccountId32::from_slice(&id[id.len() - 32..]).unwrap();

			log::info!("Fetched member: {} rank {}", account.to_ss58check(), fellow.rank);
			members.insert(
				account.clone(),
				Fellow {
					account,
					rank: fellow.rank,
					identity: None,
					github: None,
					github_links_back: false,
					score: None,
				},
			);
		}

		log::info!("Fetched {} members", members.len());
		self.members = members;
		Ok(())
	}

	async fn fetch_identities(&mut self, relay_rpc: &LightClient<PolkadotConfig>) -> Result<()> {
		log::info!("Fetching identities...");
		for (address, member) in self.members.iter_mut() {
			// Subxt has a sligly different address type...
			let r: &[u8; 32] = address.as_ref();
			let add = subxt::utils::AccountId32(*r);
			let key = polkadot::storage().identity().identity_of(add);
			let query = relay_rpc.storage().at_latest().await.unwrap().fetch(&key).await?;

			log::debug!("Identity of {}: {:?}", address.to_ss58check(), query);
			if let Some(id) = query {
				member.identity = Some(id);
				continue;
			}

			let r: &[u8; 32] = address.as_ref();
			let address = subxt::utils::AccountId32(*r);
			let key = polkadot::storage().identity().super_of(address);
			let query = relay_rpc.storage().at_latest().await.unwrap().fetch(&key).await?;
			log::debug!("Fetched super identity: {:?}", query);

			if let Some((acc, sub_name)) = query {
				log::debug!("Fetching sub identity: {:?}", data_to_str(&sub_name));
				let key = polkadot::storage().identity().identity_of(acc);
				let query = relay_rpc.storage().at_latest().await.unwrap().fetch(&key).await?;

				member.identity = query;
			}
			log::info!("Fetched identity");
		}

		Ok(())
	}

	/// Query each profile description and check if the address is mentioned.
	async fn fetch_github(&mut self) -> Result<()> {
		log::info!("Fetching github profiles...");
		let mut interval = tokio::time::interval(Duration::from_millis(1000));
		#[derive(serde::Deserialize)]
		struct Profile {
			bio: Option<String>,
		}
		let client = reqwest::Client::builder().user_agent("useragent@spam.tasty.limo").build()?;

		for (_, member) in self.members.iter_mut() {
			interval.tick().await;
			let address = member.address();

			let Some(github) = member.github() else {
				member.github_links_back = false;
				continue;
			};

			log::debug!("Fetching github profile of {}", github);
			let url = format!("https://api.github.com/users/{}", github);
			let resp = client.get(&url).send().await?;

			let profile = resp.text().await?;
			log::debug!("Github profile: {}", profile);
			let profile: Profile = serde_json::from_str(&profile)?;

			member.github_links_back = profile.bio.map_or(false, |b| b.contains(&address));
			log::debug!("{} links back: {}", address, member.github_links_back);
			log::info!("Fetched github profile");
		}

		Ok(())
	}
}

fn now_s() -> u64 {
	let start = SystemTime::now();
	let since_the_epoch = start.duration_since(UNIX_EPOCH).expect("Time went backwards");

	since_the_epoch.as_secs()
}

pub trait Human {
	fn human(&self) -> String;
}

impl<T: Human> Human for Option<T> {
	fn human(&self) -> String {
		self.as_ref().map_or("-".into(), |d| d.human())
	}
}

impl Human for Option<core::time::Duration> {
	fn human(&self) -> String {
		self.as_ref().map_or("?".into(), |d| {
			let s = d.as_secs();

			if s < 60 {
				format!("{}s", s % 60)
			} else if s < 3600 {
				format!("{}m", s / 60)
			} else if s < 86400 {
				format!("{}h", s / 3600)
			} else {
				format!("{}d", s / 86400)
			}
		})
	}
}
