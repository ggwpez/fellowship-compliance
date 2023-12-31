use core::time::Duration;
use parity_scale_codec as Codec;
use parity_scale_codec::Encode;
use sp_core::{
	crypto::{AccountId32, Ss58Codec},
	ByteArray,
};
use std::{
	collections::BTreeMap,
	io::{Read, Write},
};
use subxt::*;

#[subxt::subxt(runtime_metadata_path = "metadata/polkadot.scale")]
pub mod polkadot {}

#[subxt::subxt(runtime_metadata_path = "metadata/collectives-polkadot.scale")]
pub mod collectives {}

pub type Registration = polkadot::runtime_types::pallet_identity::types::Registration<u128>;

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
		// take the first 32 bytes
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
		self.identity.as_ref().and_then(|i| Self::reg_to_github(i))
	}
}

#[derive(Default, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub struct Fellows {
	pub members: BTreeMap<AccountId32, Fellow>,
	pub last_updated: u64,
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
		Self {
			last_updated: std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_secs(),
			.. Default::default()
		}
	}

	pub async fn load() -> Result<Self> {
		let path = std::path::Path::new("data.scale");
		if path.exists() {
			log::info!("Loading from cache...");

			match Self::try_from_cache() {
				Ok(s) => {
					log::info!("Loaded from cache");
					return Ok(Self::finalize(s))
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
		let mut s = Self::now();
		log::info!("Fetching data from remote...");

		s.fetch_fellows().await?;
		s.fetch_identities().await?;
		s.fetch_github().await?;

		// Store in a file for faster restarts if it crashed
		let mut file = std::fs::File::create("data.scale")?;
		let data = parity_scale_codec::Encode::encode(&s);
		file.write_all(&data)?;
		log::info!("Data written to data.json");

		Ok(s.finalize())
	}

	async fn fetch_fellows(&mut self) -> Result<()> {
		let mut interval = tokio::time::interval(Duration::from_millis(2000));
		log::info!("Initializing chain client...");
		let url = "wss://polkadot-collectives-rpc.dwellir.com:443";
		let client = Client::from_url(&url).await.map_err(|e| format!("Failed to connect to {}: {}", url, e))?;

		log::info!("Fetching collectives data...");
		let mut members = BTreeMap::new();
		let key = collectives::storage().fellowship_collective().members_iter();
		let mut query = client.storage().at_latest().await.unwrap().iter(key).await?;

		while let Some(Ok((id, fellow))) = query.next().await {
			interval.tick().await;
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

		self.members = members;
		Ok(())
	}

	async fn fetch_identities(&mut self) -> Result<()> {
		let mut interval = tokio::time::interval(Duration::from_millis(2000));
		let url = "wss://rpc.polkadot.io:443";
		let client = Client::from_url(&url).await?;
		log::info!("Fetching identities...");
		for (address, member) in self.members.iter_mut() {
			interval.tick().await;
			// Subxt has a sligly different address type...
			let r: &[u8; 32] = address.as_ref();
			let add = subxt::utils::AccountId32(*r);
			let key = polkadot::storage().identity().identity_of(add);
			let query = client.storage().at_latest().await.unwrap().fetch(&key).await?;

			log::debug!("Identity of {}: {:?}", address.to_ss58check(), query);
			if let Some(id) = query {
				member.identity = Some(id);
				continue;
			}

			interval.tick().await;
			let r: &[u8; 32] = address.as_ref();
			let address = subxt::utils::AccountId32(*r);
			let key = polkadot::storage().identity().super_of(address);
			let query = client.storage().at_latest().await.unwrap().fetch(&key).await?;
			log::debug!("Fetched super identity: {:?}", query);

			if let Some((acc, sub_name)) = query {
				interval.tick().await;
				log::debug!("Fetching sub identity: {:?}", data_to_str(&sub_name));
				let key = polkadot::storage().identity().identity_of(acc);
				let query = client.storage().at_latest().await.unwrap().fetch(&key).await?;

				member.identity = query;
			}
			log::info!("Fetched identity");
		}

		Ok(())
	}

	/// Query each profile description and check if the address is mentioned.
	async fn fetch_github(&mut self) -> Result<()> {
		log::info!("Fetching github profiles...");
		let mut interval = tokio::time::interval(Duration::from_millis(2000));
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
