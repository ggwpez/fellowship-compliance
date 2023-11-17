use actix_web::HttpResponse;
use sailfish::TemplateOnce;

#[derive(TemplateOnce)]
#[template(path = "members.stpl")]
pub struct Members<'a> {
	pub members: &'a crate::chain::Fellows,
}

pub(crate) fn http_500(msg: String) -> HttpResponse {
	HttpResponse::InternalServerError()
		.content_type("text/html; charset=utf-8")
		.body(msg)
}

pub(crate) fn http_200<T>(msg: T) -> HttpResponse
where
	String: std::convert::From<T>,
{
	let msg: String = msg.into();
	HttpResponse::Ok().content_type("text/html; charset=utf-8").body(msg)
}
