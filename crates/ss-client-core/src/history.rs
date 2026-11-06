//! Pinned HTTPS pagination and protobuf-JSON decoding.
use crate::{
    Client, ClientEvent, ConnectionState, Error, ServerAddr, connection::DEADLINE, state::id,
};
use base64::Engine;
use serde::Deserialize;
use ss_core::Id;
use ss_proto::v1;

const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
impl Client {
    /// Fetch 50 messages older than `before`, or the newest page when None.
    /// Redirects and environment proxies are disabled. HTTP and QUIC must
    /// use the same trust policy (the same leaf only in pinned mode).
    /// Pages are retained until reconnect.
    /// If a live event races the request, returns `StateChanged` for a retry.
    pub async fn load_older(
        &self,
        channel: Id,
        before: Option<Id>,
    ) -> Result<Vec<v1::Message>, Error> {
        self.load_page(channel, before, None).await
    }

    /// Load the authorized history window around a reply/link target over verified HTTPS.
    pub async fn load_around(&self, channel: Id, around: Id) -> Result<Vec<v1::Message>, Error> {
        self.load_page(channel, None, Some(around)).await
    }

    async fn load_page(
        &self,
        channel: Id,
        before: Option<Id>,
        around: Option<Id>,
    ) -> Result<Vec<v1::Message>, Error> {
        let revision = {
            let state = self.store.read();
            if state.connection != ConnectionState::Connected {
                return Err(Error::Disconnected);
            }
            state.revision
        };
        let session = self.auth.read().clone().ok_or(Error::Disconnected)?;
        let page = fetch_page(
            &self.address,
            session.trust,
            &session.token,
            channel,
            before,
            None,
            around,
        )
        .await?;
        self.store.history(channel, revision, page.clone())?;
        let _ = self.events.send(ClientEvent::MessagesChanged { channel });
        Ok(page)
    }
}

pub(crate) async fn fetch_page(
    address: &ServerAddr,
    trust: ss_net::pins::TrustPolicy,
    token: &str,
    channel: Id,
    before: Option<Id>,
    after: Option<Id>,
    around: Option<Id>,
) -> Result<Vec<v1::Message>, Error> {
    let http = reqwest::Client::builder()
        .use_preconfigured_tls(ss_net::client::tls_config(trust)?)
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DEADLINE)
        .build()?;
    let host = if address.host.contains(':') {
        format!("[{}]", address.host)
    } else {
        address.host.clone()
    };
    let mut url = format!(
        "https://{host}:{}/api/v1/channels/{channel}/messages",
        address.https_port
    );
    if let Some(around) = around {
        url.push_str("/around/");
        url.push_str(&around.to_string());
    }
    let mut request = http.get(url).bearer_auth(token).query(&[("limit", "50")]);
    if let Some(before) = before {
        request = request.query(&[("before", before.to_string())]);
    }
    if let Some(after) = after {
        request = request.query(&[("after", after.to_string())]);
    }
    let mut response = request.send().await?.error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_PAGE_BYTES {
            return Err(Error::Protocol("history page too large"));
        }
        body.extend_from_slice(&chunk);
    }
    decode(&body, channel)
}

pub(crate) fn decode(body: &[u8], channel: Id) -> Result<Vec<v1::Message>, Error> {
    let rows: Vec<Message> = serde_json::from_slice(body)?;
    if rows.len() > 50 {
        return Err(Error::Protocol("too many history messages"));
    }
    let mut page = Vec::with_capacity(rows.len());
    for row in rows {
        let message = row.into_wire()?;
        if id(&message.channel_id)? != channel {
            return Err(Error::Protocol("history belongs to another channel"));
        }
        let _ = id(&message.id)?;
        page.push(message);
    }
    page.sort_by(|a, b| a.id.cmp(&b.id));
    if page.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(Error::Protocol("duplicate history id"));
    }
    Ok(page)
}
fn bytes(value: &str) -> Result<bytes::Bytes, Error> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| Error::Protocol("invalid base64 id"))?;
    let _ = id(&bytes)?;
    Ok(bytes.into())
}
fn optional(value: Option<String>) -> Result<Option<bytes::Bytes>, Error> {
    value.map(|v| bytes(&v)).transpose()
}
fn number<T: std::str::FromStr>(value: &str) -> Result<T, Error> {
    value
        .parse()
        .map_err(|_| Error::Protocol("invalid JSON integer"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    id: String,
    channel_id: String,
    author: Option<User>,
    content: String,
    reply_to: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
    #[serde(default)]
    mentions: Vec<String>,
    #[serde(default)]
    link_previews: Vec<Preview>,
    created_at: String,
    edited_at: Option<String>,
    webhook_id: Option<String>,
    webhook_name: Option<String>,
}
impl Message {
    fn into_wire(self) -> Result<v1::Message, Error> {
        Ok(v1::Message {
            id: bytes(&self.id)?,
            channel_id: bytes(&self.channel_id)?,
            author: self.author.map(User::into_wire).transpose()?,
            content: self.content,
            reply_to: optional(self.reply_to)?,
            attachments: self
                .attachments
                .into_iter()
                .map(Attachment::into_wire)
                .collect::<Result<_, _>>()?,
            mentions: self
                .mentions
                .iter()
                .map(|v| bytes(v))
                .collect::<Result<_, _>>()?,
            link_previews: self
                .link_previews
                .into_iter()
                .map(Preview::into_wire)
                .collect(),
            created_at: number(&self.created_at)?,
            edited_at: self.edited_at.map(|v| number(&v)).transpose()?,
            webhook_id: optional(self.webhook_id)?,
            webhook_name: self.webhook_name,
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    id: String,
    username: String,
    display_name: String,
    avatar_attachment_id: Option<String>,
    kind: String,
}
impl User {
    fn into_wire(self) -> Result<v1::User, Error> {
        Ok(v1::User {
            id: bytes(&self.id)?,
            username: self.username,
            display_name: self.display_name,
            avatar_attachment_id: optional(self.avatar_attachment_id)?,
            kind: match self.kind.as_str() {
                "USER_KIND_USER" => v1::UserKind::User,
                "USER_KIND_BOT" => v1::UserKind::Bot,
                _ => return Err(Error::Protocol("invalid user kind")),
            }
            .into(),
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attachment {
    id: String,
    filename: String,
    mime: String,
    size: String,
    width: Option<u32>,
    height: Option<u32>,
}
impl Attachment {
    fn into_wire(self) -> Result<v1::Attachment, Error> {
        Ok(v1::Attachment {
            id: bytes(&self.id)?,
            filename: self.filename,
            mime: self.mime,
            size: number(&self.size)?,
            width: self.width,
            height: self.height,
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Preview {
    url: String,
    title: Option<String>,
    description: Option<String>,
    image_url: Option<String>,
    site_name: Option<String>,
}
impl Preview {
    fn into_wire(self) -> v1::LinkPreview {
        v1::LinkPreview {
            url: self.url,
            title: self.title,
            description: self.description,
            image_url: self.image_url,
            site_name: self.site_name,
        }
    }
}
