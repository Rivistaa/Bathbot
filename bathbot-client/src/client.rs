use bathbot_model::TwitchData;
use bytes::Bytes;
use eyre::{Result, WrapErr};
use http::{
    header::{AUTHORIZATION, CONTENT_LENGTH, COOKIE},
    Response,
};
use hyper::{
    client::{connect::dns::GaiResolver, Client as HyperClient, HttpConnector},
    header::{CONTENT_TYPE, USER_AGENT},
    Body, Method, Request,
};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use leaky_bucket_lite::LeakyBucket;
use tokio::time::Duration;

use crate::{multipart::Multipart, ClientError, Site, MY_USER_AGENT};

pub(crate) type InnerClient = HyperClient<HttpsConnector<HttpConnector<GaiResolver>>, Body>;

pub struct Client {
    pub(crate) client: InnerClient,
    osu_session: &'static str,
    twitch: Option<TwitchData>,
    ratelimiters: [LeakyBucket; 12],
}

impl Client {
    /// `twitch_login` consists of `(twitch client id, twitch token)`
    pub async fn new(
        osu_session: &'static str,
        twitch_login: Option<(&str, &str)>,
    ) -> Result<Self> {
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();

        let client = HyperClient::builder().build(connector);

        let twitch = match twitch_login {
            Some((twitch_client_id, twitch_token)) => {
                Self::get_twitch_token(&client, twitch_client_id, twitch_token)
                    .await
                    .wrap_err("failed to get twitch token")
                    .map(Some)?
            }
            None => None,
        };

        let ratelimiter = |per_second| {
            LeakyBucket::builder()
                .max(per_second)
                .tokens(per_second)
                .refill_interval(Duration::from_millis(1000 / per_second as u64))
                .refill_amount(1)
                .build()
        };

        let ratelimiters = [
            ratelimiter(2),  // DiscordAttachment
            ratelimiter(2),  // Huismetbenen
            ratelimiter(2),  // Osekai
            ratelimiter(10), // OsuAvatar
            ratelimiter(10), // OsuBadge
            ratelimiter(2),  // OsuHiddenApi
            ratelimiter(2),  // OsuMapFile
            ratelimiter(10), // OsuMapsetCover
            ratelimiter(2),  // OsuStats
            ratelimiter(2),  // OsuTracker
            ratelimiter(1),  // Respektive
            ratelimiter(5),  // Twitch
        ];

        Ok(Self {
            client,
            osu_session,
            ratelimiters,
            twitch,
        })
    }

    pub(crate) async fn ratelimit(&self, site: Site) {
        self.ratelimiters[site as usize].acquire_one().await
    }

    pub(crate) async fn make_get_request(
        &self,
        url: impl AsRef<str>,
        site: Site,
    ) -> Result<Bytes, ClientError> {
        let url = url.as_ref();
        trace!("GET request of url {url}");

        let req = Request::builder()
            .uri(url)
            .method(Method::GET)
            .header(USER_AGENT, MY_USER_AGENT);

        let req = match site {
            Site::OsuHiddenApi => req.header(COOKIE, format!("osu_session={}", self.osu_session)),
            Site::Twitch => {
                let twitch = self.twitch.as_ref().ok_or(ClientError::MissingTwitch)?;

                req.header("Client-ID", twitch.client_id.clone())
                    .header(AUTHORIZATION, format!("Bearer {}", twitch.oauth_token))
            }
            _ => req,
        };

        let req = req
            .body(Body::empty())
            .wrap_err("failed to build GET request")?;

        self.ratelimit(site).await;

        let response = self
            .client
            .request(req)
            .await
            .wrap_err("failed to receive GET response")?;

        Self::error_for_status(response, url).await
    }

    pub(crate) async fn make_post_request(
        &self,
        url: impl AsRef<str>,
        site: Site,
        form: Multipart,
    ) -> Result<Bytes, ClientError> {
        let url = url.as_ref();
        trace!("POST request of url {url}");

        let content_type = format!("multipart/form-data; boundary={}", form.boundary());
        let form = form.finish();

        let req = Request::builder()
            .method(Method::POST)
            .uri(url)
            .header(USER_AGENT, MY_USER_AGENT)
            .header(CONTENT_TYPE, content_type)
            .header(CONTENT_LENGTH, form.len())
            .body(Body::from(form))
            .wrap_err("failed to build POST request")?;

        self.ratelimit(site).await;

        let response = self
            .client
            .request(req)
            .await
            .wrap_err("failed to receive POST response")?;

        Self::error_for_status(response, url).await
    }

    pub(crate) async fn error_for_status(
        response: Response<Body>,
        url: &str,
    ) -> Result<Bytes, ClientError> {
        let status = response.status();

        match status.as_u16() {
            200..=299 => hyper::body::to_bytes(response.into_body())
                .await
                .wrap_err("failed to extract response bytes")
                .map_err(ClientError::Report),
            400 => Err(ClientError::BadRequest),
            404 => Err(ClientError::NotFound),
            429 => Err(ClientError::Ratelimited),
            _ => Err(eyre!("failed with status code {status} when requesting url {url}").into()),
        }
    }
}