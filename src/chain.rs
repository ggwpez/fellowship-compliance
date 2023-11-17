use subxt::*;
use sp_core::crypto::AccountId32;
use parity_scale_codec::Encode;
use std::collections::BTreeMap;
use core::time::Duration;
use sp_core::crypto::Ss58Codec;
use sp_core::ByteArray;
use parity_scale_codec as Codec;
use std::io::Write;
use std::io::Read;

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
        self.account.to_ss58check_with_version(sp_core::crypto::Ss58AddressFormat::custom(0))
    }

    pub fn name(&self) -> Option<String> {
        self.identity.as_ref().and_then(|i| data_to_str(&i.info.display))
    }

    pub fn verified(&self) -> bool {
        use polkadot::runtime_types::pallet_identity::types::Judgement::*;
        self.identity.as_ref().map(|r| {
            for (_, j) in r.judgements.0.iter() {
                if matches!(j, Reasonable | KnownGood) {
                    return true;
                }
            }
            false
        }).unwrap_or(false)
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
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type Client = OnlineClient<subxt::SubstrateConfig>;

impl Fellows {
    pub async fn load() -> Result<Self> {
        let path = std::path::Path::new("data.scale");
        if path.exists() {
            log::info!("Loading from cache...");
            
            match Self::try_from_cache() {
                Ok(s) => {
                    log::info!("Loaded from cache");
                    return Ok(s)
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

    pub async fn fetch() -> Result<Self> {
        let mut s = Self::default();
        log::info!("Fetching data from remote...");

        s.fetch_fellows().await?;
        s.fetch_identities().await?;
        s.fetch_github().await?;
        
        // Store in a file for faster restarts if it crashed
        let mut file = std::fs::File::create("data.scale")?;
        let data = parity_scale_codec::Encode::encode(&s);
        file.write_all(&data)?;
        log::info!("Data written to data.json");
        
        Ok(s)
    }

    async fn fetch_fellows(&mut self) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        log::info!("Initializing chain client...");
        let url = "wss://polkadot-collectives-rpc.polkadot.io:443";
        let client = Client::from_url(&url).await?;
        log::info!("Chain RPC connected");
        
        log::info!("Fetching data...");
        let mut members = BTreeMap::new();
        let key = collectives::storage().fellowship_collective().members_iter();
        let mut query = client.storage().at_latest().await.unwrap().iter(key).await?;
        
        while let Some(Ok((id, fellow))) = query.next().await {        
            interval.tick().await;
            let account = AccountId32::from_slice(&id[id.len()-32..]).unwrap();
            
            log::debug!("Fetched member: {} rank {}", account.to_ss58check(), fellow.rank);
            members.insert(account.clone(), Fellow { account, rank: fellow.rank, identity: None, github: None, github_links_back: false, score: None });
        }

        self.members = members;
        Ok(())
    }

    async fn fetch_identities(&mut self) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
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
            member.identity = query;
            // TODO sub identities
            //let key = polkadot::storage().identity().subs_of(address);
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
        let client = reqwest::Client::builder()
            .user_agent("useragent@spam.tasty.limo")
            .build()?;

        for (_, member) in self.members.iter_mut() {
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
            interval.tick().await;
        }

        Ok(())
    }
}
