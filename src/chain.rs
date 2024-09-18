// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: Oliver Tale-Yazdi <oliver@tasty.limo>

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
	PolkadotConfig, *,
	lightclient::LightClient,
};

#[subxt::subxt(runtime_metadata_path = "metadata/polkadot.scale")]
pub mod polkadot {}

#[subxt::subxt(runtime_metadata_path = "metadata/collectives-polkadot.scale")]
pub mod collectives {}

#[subxt::subxt(runtime_metadata_path = "metadata/people-polkadot.scale")]
pub mod people {}

const POLKADOT_SPEC: &str = include_str!("../metadata/polkadot.json");
const COLLECTIVES_SPEC: &str = include_str!("../metadata/collectives-polkadot.json");
const PEOPLE_SPEC: &str = include_str!("../metadata/people-polkadot.json");

pub type Registration = people::runtime_types::pallet_identity::types::Registration<
	u128,
	people::runtime_types::people_polkadot_runtime::people::IdentityInfo,
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

fn data_to_str(data: &people::runtime_types::pallet_identity::types::Data) -> Option<String> {
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
		use people::runtime_types::pallet_identity::types::Judgement::*;
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
		data_to_str(&r.info.github)
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

		let (polkadot_api, collectives_api, people_api) = Self::build_clients().await?;
		log::info!("Clients set up");

		s.fetch_fellows(&collectives_api).await?;
		s.fetch_identities(&people_api).await?;
		std::mem::drop((polkadot_api, collectives_api, people_api));
		s.fetch_github().await?;

		// Store in a file for faster restarts if it crashed
		let mut file = std::fs::File::create("data.scale")?;
		let data = parity_scale_codec::Encode::encode(&s);
		file.write_all(&data)?;
		log::info!("Data written to data.scale");

		Ok(s.finalize())
	}

	// (polkadot, collectives, people)
	async fn build_clients() -> Result<(OnlineClient<PolkadotConfig>, OnlineClient<PolkadotConfig>, OnlineClient<PolkadotConfig>)> {
		//let (lightclient, polkadot_rpc) = LightClient::relay_chain(POLKADOT_SPEC)?;
		//let collective_rpc = lightclient.parachain(COLLECTIVES_SPEC)?;
		//let people_rpc = lightclient.parachain(PEOPLE_SPEC)?;
		let polkadot_rpc = std::env::var("POLKADOT_RPC").expect("POLKADOT_RPC not set");
		let collective_rpc = std::env::var("COLLECTIVES_RPC").expect("COLLECTIVES_RPC not set");
		let people_rpc = std::env::var("PEOPLE_RPC").expect("PEOPLE_RPC not set");

		let polkadot_api = OnlineClient::<PolkadotConfig>::from_url(polkadot_rpc).await?;
		let collective_api = OnlineClient::<PolkadotConfig>::from_url(collective_rpc).await?;
		let people_api = OnlineClient::<PolkadotConfig>::from_url(people_rpc).await?;

		Ok((polkadot_api, collective_api, people_api))
	}

	async fn fetch_fellows(&mut self, collectives_rpc: &OnlineClient<PolkadotConfig>) -> Result<()> {
		log::info!("Fetching collectives data...");
		let mut members = BTreeMap::new();
		let key = collectives::storage().fellowship_collective().members_iter();
		let mut query = collectives_rpc.storage().at_latest().await.unwrap().iter(key).await?;

		while let Some(Ok(pair)) = query.next().await {
			let id = pair.key_bytes;
			let fellow = pair.value;
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

	async fn fetch_identities(&mut self, relay_rpc: &OnlineClient<PolkadotConfig>) -> Result<()> {
		log::info!("Fetching identities...");
		for (address, member) in self.members.iter_mut() {
			// Subxt has a sligly different address type...
			let r: &[u8; 32] = address.as_ref();
			let add = subxt::utils::AccountId32(*r);
			let key = people::storage().identity().identity_of(add);
			let query = relay_rpc.storage().at_latest().await.unwrap().fetch(&key).await?;

			log::debug!("Identity of {}: {:?}", address.to_ss58check(), query);
			if let Some((id, _)) = query {
				member.identity = Some(id);
				continue;
			}

			let r: &[u8; 32] = address.as_ref();
			let address = subxt::utils::AccountId32(*r);
			let key = people::storage().identity().super_of(address);
			let query = relay_rpc.storage().at_latest().await.unwrap().fetch(&key).await?;
			log::debug!("Fetched super identity: {:?}", query);

			if let Some((acc, sub_name)) = query {
				log::debug!("Fetching sub identity: {:?}", data_to_str(&sub_name));
				let key = people::storage().identity().identity_of(acc);
				let query = relay_rpc.storage().at_latest().await.unwrap().fetch(&key).await?;

				member.identity = query.map(|(id, _)| id);
			}
			log::info!("Fetched identity");
		}

		Ok(())
	}

	/// Query each profile description and check if the address is mentioned.
	async fn fetch_github(&mut self) -> Result<()> {
		log::info!("Fetching github profiles...");

		let mut builder = octocrab::Octocrab::builder();
		if let Ok(token) = std::env::var("GITHUB_TOKEN") {
			log::info!("Using GITUB_TOKEN for authentication");
			builder = builder.personal_token(token);
		} else {
			log::warn!("No GITHUB_TOKEN set. Rate limits will be lower");
		}
		let octo = builder.build()?;

		for (_, member) in self.members.iter_mut() {
			let address = member.address();

			let Some(github) = member.github() else {
				member.github_links_back = false;
				continue;
			};

			let github = github.replace("@", "");
			log::info!("Fetching github profile of {}", &github);
			if let Ok(profile) = octo.users(github.trim()).profile().await {
				member.github_links_back = profile.bio.map_or(false, |b| b.contains(&address));
				log::debug!("{} links back: {}", address, member.github_links_back);
			}
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
