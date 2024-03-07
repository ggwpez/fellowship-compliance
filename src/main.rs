#![allow(dead_code)] // The caching crate produces "unused" functions…
#![allow(unused_imports)]

use actix_files as fs;
use actix_web::{
	get,
	http::header::{CacheControl, CacheDirective},
	middleware,
	middleware::Logger,
	web,
	web::Data,
	App, HttpRequest, HttpResponse, HttpServer, Responder, Result,
};
use badge_maker::BadgeBuilder;
use cached::proc_macro::cached;
use clap::Parser;
use core::time::Duration;
use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod};
use std::{
	collections::{BTreeMap, BTreeSet},
	path::{Path, PathBuf},
	process::Command,
	sync::RwLock,
};
use subxt::utils::AccountId32;

mod chain;
mod html;
use html::*;

#[derive(Debug, Parser, Clone)]
#[clap(author)]
pub(crate) struct MainCmd {
	#[clap(long = "static", short, default_value = "static")]
	pub static_path: PathBuf,

	#[clap(long, short, default_value = "127.0.0.1")]
	pub endpoint: String,

	#[clap(long, short, default_value = "8080")]
	pub port: u16,

	/// PEM format cert.
	#[clap(long, requires("key"))]
	pub cert: Option<String>,

	/// PEM format key.
	#[clap(long, requires("cert"))]
	pub key: Option<String>,
}

#[derive(Default)]
pub struct State {
	fellows: chain::Fellows,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
	std::env::set_var("RUST_BACKTRACE", "1");
	env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));
	let cmd = MainCmd::parse();

	let endpoint = format!("{}:{}", cmd.endpoint, cmd.port);
	log::info!("Listening to http://{}", endpoint);
	let static_path = cmd.static_path.into_os_string();

	// check that static_path is a dir
	if !Path::new(&static_path).is_dir() {
		return Err(std::io::Error::new(
			std::io::ErrorKind::Other,
			format!("Web root path '{:?}' is not a directory", static_path),
		));
	}

	let data = Data::new(RwLock::new(State::default()));
	let d2 = data.clone();

	let server = HttpServer::new(move || {
		App::new()
			.app_data(Data::clone(&data))
			.wrap(middleware::Compress::default())
			.wrap(Logger::new("%a %r %s %b %{Referer}i %Ts"))
			.service(index)
			.service(version)
			.service(fs::Files::new("/static", &static_path).show_files_listing())
	})
	.workers(6);

	// Use this single-threaded runtime for spawning since out state is not `Send`.
	actix_web::rt::spawn(async move {
		let mut interval = tokio::time::interval(Duration::from_secs(60 * 60 * 2)); // 2 hrs
		{
			interval.tick().await;
			let fellows = chain::Fellows::load().await.unwrap();
			d2.write().unwrap().fellows = fellows; // TODO timeout
		}

		loop {
			interval.tick().await;
			let fellows = chain::Fellows::fetch().await.unwrap();
			d2.write().unwrap().fellows = fellows;
		}
	});

	let bound_server = if let Some(cert) = cmd.cert {
		let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
		builder
			.set_private_key_file(cmd.key.expect("Checked by clap"), SslFiletype::PEM)
			.unwrap();
		builder.set_certificate_chain_file(cert).unwrap();
		server.bind_openssl(endpoint, builder)
	} else {
		server.bind(endpoint)
	};

	bound_server?.run().await
}

#[get("/")]
async fn index(data: Data<RwLock<State>>) -> impl Responder {
	use sailfish::TemplateOnce;
	http_200(
		html::Members::from_members(&data.read().unwrap().fellows)
			.render_once()
			.unwrap(),
	)
}

#[get("/version")]
async fn version() -> impl Responder {
	http_200(env!("CARGO_PKG_VERSION"))
}
