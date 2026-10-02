//! Talking to the models: Ollama on this machine, NVIDIA NIM in the cloud.
//!
//! # Local first, and not only for cost
//!
//! The prompt contains the titles and hosts of every open tab. On a security
//! researcher's machine that is a browsing history, and a browsing history is not
//! something to ship to a third party as a side effect of managing memory. The
//! local tier keeps it on the box; the cloud tier is opt-in and off by default
//! even when a key is present.
//!
//! # Structured output: two services, two different recipes
//!
//! Both were established by measurement, not documentation, and neither is what
//! the obvious reading of the API would suggest.
//!
//! **Ollama** takes a JSON Schema in its native `format` field, and that works —
//! *provided* `think` is not also sent. Passing `think: false` alongside `format`
//! silently disables schema enforcement on Ollama 0.20.2 and
//! `nemotron-3-nano:4b` answers in prose. Omit it and the schema is honoured.
//!
//! **NIM** is OpenAI-compatible, but `response_format.json_schema` is the wrong
//! choice. Measured against `nemotron-3-super-120b-a12b`, schema mode emitted
//! `{"tabs_to_close": [1]` followed by whitespace until the token limit — a
//! truncated object that never closes. What works is plain `json_object` mode
//! plus `chat_template_kwargs.thinking = false`: valid JSON with the right field
//! names in half a second.
//!
//! `json_object` constrains only that the reply *is* JSON, not its shape, so the
//! field names rest on the prompt and on lenient parsing. That is why
//! [`crate::prompt::Recommendation`] tolerates aliases and string ids, and why
//! [`crate::prompt::Recommendation::clamp_all`] is not optional.

use std::time::Duration;

use serde::Deserialize;

use crate::prompt::{self, Recommendation};
use crate::secret::Secret;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Transport(String),
    #[error("{service} returned {status}: {body}")]
    Status {
        service: &'static str,
        status: u16,
        body: String,
    },
    #[error("could not read the reply: {0}")]
    Reply(String),
    #[error("{0} is not configured")]
    NotConfigured(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

fn transport(e: reqwest::Error) -> Error {
    // reqwest's Display includes the URL but never the headers, so no key can
    // reach an error message by this path.
    Error::Transport(e.to_string())
}

// ── Ollama, on this machine ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Ollama {
    http: reqwest::Client,
    host: String,
    pub model: String,
    pub embed_model: String,
}

#[derive(Deserialize)]
struct OllamaChat {
    message: OllamaMessage,
}

#[derive(Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct OllamaEmbed {
    #[serde(default)]
    embeddings: Vec<Vec<f32>>,
}

#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaTag>,
}

#[derive(Deserialize)]
struct OllamaTag {
    #[serde(default)]
    name: String,
}

impl Ollama {
    pub fn new(host: &str, model: &str, embed_model: &str, timeout: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(transport)?;
        Ok(Ollama {
            http,
            host: host.trim_end_matches('/').to_string(),
            model: model.to_string(),
            embed_model: embed_model.to_string(),
        })
    }

    /// Which models this Ollama has pulled.
    pub async fn models(&self) -> Result<Vec<String>> {
        let r = self
            .http
            .get(format!("{}/api/tags", self.host))
            .send()
            .await
            .map_err(transport)?;
        let status = r.status();
        let body = r.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Status {
                service: "ollama",
                status: status.as_u16(),
                body: body.chars().take(200).collect(),
            });
        }
        let tags: OllamaTags = serde_json::from_str(&body).map_err(|e| Error::Reply(e.to_string()))?;
        Ok(tags.models.into_iter().map(|m| m.name).collect())
    }

    /// Whether both configured models are actually present.
    ///
    /// Checked rather than assumed: a missing model makes Ollama pull it mid-
    /// request, which turns a one-second analysis into a multi-gigabyte download
    /// on a machine already under memory pressure.
    pub async fn ready(&self) -> (bool, Vec<String>) {
        let Ok(have) = self.models().await else {
            return (false, Vec::new());
        };
        let present = |want: &str| {
            have.iter()
                .any(|h| h == want || h.split(':').next() == want.split(':').next())
        };
        let missing: Vec<String> = [self.model.as_str(), self.embed_model.as_str()]
            .iter()
            .filter(|m| !present(m))
            .map(|m| m.to_string())
            .collect();
        (missing.is_empty(), missing)
    }

    pub async fn recommend(&self, system: &str, user: &str) -> Result<Recommendation> {
        let body = serde_json::json!({
            "model": self.model,
            "stream": false,
            "format": prompt::reply_schema(),
            // `think: false` is deliberately NOT sent. Measured on Ollama
            // 0.20.2: passing it alongside `format` silently disables schema
            // enforcement, and nemotron-3-nano:4b then answers in prose. With it
            // omitted the same request returns `{"nums":[7,12]}` exactly as the
            // schema demands. Any reasoning preamble is stripped on the way out.
            "options": {"temperature": 0, "num_predict": 1024},
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });

        let r = self
            .http
            .post(format!("{}/api/chat", self.host))
            .json(&body)
            .send()
            .await
            .map_err(transport)?;
        let status = r.status();
        let text = r.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Status {
                service: "ollama",
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }

        let parsed: OllamaChat =
            serde_json::from_str(&text).map_err(|e| Error::Reply(e.to_string()))?;
        let mut rec = prompt::extract_json(prompt::strip_thinking(&parsed.message.content))
            .map_err(Error::Reply)?;
        rec.tier = "local".into();
        Ok(rec)
    }

    /// Embed documents (tab descriptions).
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed = crate::rerank::prefix_documents(&self.embed_model, texts);
        self.embed_raw(&prefixed).await
    }

    /// Embed a search query (the user's goal).
    ///
    /// Queries and documents must be embedded differently or the similarities
    /// mean nothing. Both models in use require it, in different ways — see
    /// [`crate::rerank::prefix_query`].
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let q = crate::rerank::prefix_query(&self.embed_model, text);
        Ok(self.embed_raw(&[q]).await?.into_iter().next().unwrap_or_default())
    }

    async fn embed_raw(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({"model": self.embed_model, "input": texts});
        let r = self
            .http
            .post(format!("{}/api/embed", self.host))
            .json(&body)
            .send()
            .await
            .map_err(transport)?;
        let status = r.status();
        let text = r.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Status {
                service: "ollama",
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }
        let parsed: OllamaEmbed =
            serde_json::from_str(&text).map_err(|e| Error::Reply(e.to_string()))?;
        Ok(parsed.embeddings)
    }
}

// ── NVIDIA NIM, in the cloud ────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Nim {
    http: reqwest::Client,
    base: String,
    key: Secret,
    pub model: String,
    pub embed_model: String,
}

#[derive(Deserialize)]
struct OpenAiChat {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct OpenAiEmbeddings {
    #[serde(default)]
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    #[serde(default)]
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

impl Nim {
    pub fn new(
        base: &str,
        key: Secret,
        model: &str,
        embed_model: &str,
        timeout: Duration,
    ) -> Result<Self> {
        if key.is_empty() {
            return Err(Error::NotConfigured("NVIDIA NIM (no API key)"));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(transport)?;
        Ok(Nim {
            http,
            base: base.trim_end_matches('/').to_string(),
            key,
            model: model.to_string(),
            embed_model: embed_model.to_string(),
        })
    }

    /// Which key is in use, safe to log.
    pub fn key_fingerprint(&self) -> String {
        self.key.fingerprint()
    }

    pub async fn models(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Models {
            #[serde(default)]
            data: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            #[serde(default)]
            id: String,
        }

        let r = self
            .http
            .get(format!("{}/v1/models", self.base))
            .bearer_auth(self.key.expose())
            .send()
            .await
            .map_err(transport)?;
        let status = r.status();
        let text = r.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Status {
                service: "nvidia",
                status: status.as_u16(),
                body: text.chars().take(200).collect(),
            });
        }
        let parsed: Models = serde_json::from_str(&text).map_err(|e| Error::Reply(e.to_string()))?;
        Ok(parsed.data.into_iter().map(|m| m.id).collect())
    }

    pub async fn recommend(&self, system: &str, user: &str) -> Result<Recommendation> {
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0,
            "max_tokens": 1024,
            // Not `json_schema` — see the module note. Schema mode truncates on
            // this model; json_object plus no thinking is what actually returns a
            // usable object.
            "response_format": {"type": "json_object"},
            "chat_template_kwargs": {"thinking": false},
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });

        let r = self
            .http
            .post(format!("{}/v1/chat/completions", self.base))
            .bearer_auth(self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(transport)?;
        let status = r.status();
        let text = r.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Status {
                service: "nvidia",
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }

        let parsed: OpenAiChat =
            serde_json::from_str(&text).map_err(|e| Error::Reply(e.to_string()))?;
        let content = parsed
            .choices
            .first()
            .map(|c| c.message.content.as_str())
            .unwrap_or("");
        let mut rec =
            prompt::extract_json(prompt::strip_thinking(content)).map_err(Error::Reply)?;
        rec.tier = "cloud".into();
        Ok(rec)
    }

    /// Embed with NVIDIA's retrieval models.
    ///
    /// `input_type` is required by these models and is not part of the OpenAI
    /// shape: a query and the documents it searches must be embedded differently
    /// or the similarities are meaningless.
    pub async fn embed(&self, texts: &[String], input_type: &str) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "model": self.embed_model,
            "input": texts,
            "input_type": input_type,
            "encoding_format": "float",
        });

        let r = self
            .http
            .post(format!("{}/v1/embeddings", self.base))
            .bearer_auth(self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(transport)?;
        let status = r.status();
        let text = r.text().await.map_err(transport)?;
        if !status.is_success() {
            return Err(Error::Status {
                service: "nvidia",
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }

        let parsed: OpenAiEmbeddings =
            serde_json::from_str(&text).map_err(|e| Error::Reply(e.to_string()))?;
        // The API does not promise ordering, and a reordered embedding list
        // silently pairs every tab with the wrong vector.
        let mut rows: Vec<(usize, Vec<f32>)> = parsed
            .data
            .into_iter()
            .map(|d| (d.index, d.embedding))
            .collect();
        rows.sort_by_key(|(i, _)| *i);
        Ok(rows.into_iter().map(|(_, v)| v).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nim_client_without_a_key_refuses_to_exist() {
        let e = Nim::new(
            "https://integrate.api.nvidia.com",
            Secret::new(""),
            "m",
            "e",
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(e, Error::NotConfigured(_)), "{e:?}");
    }

    #[test]
    fn a_nim_clients_debug_output_hides_the_key() {
        let c = Nim::new(
            "https://integrate.api.nvidia.com",
            Secret::new("nvapi-secretsecretsecretsecret123456ZZTOP9"),
            "m",
            "e",
            Duration::from_secs(5),
        )
        .unwrap();
        let printed = format!("{c:?}");
        assert!(!printed.contains("secretsecret"), "{printed}");
        assert_eq!(c.key_fingerprint(), "***ZZTOP9");
    }

    #[test]
    fn trailing_slashes_on_the_host_do_not_double_up() {
        let o = Ollama::new("http://127.0.0.1:11434/", "m", "e", Duration::from_secs(5)).unwrap();
        assert_eq!(o.host, "http://127.0.0.1:11434");
    }

    #[tokio::test]
    async fn embedding_nothing_makes_no_request() {
        let o = Ollama::new("http://127.0.0.1:1", "m", "e", Duration::from_millis(50)).unwrap();
        // Would fail if it tried to connect to a dead port.
        assert!(o.embed(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_ollama_is_a_transport_error_not_a_panic() {
        let o = Ollama::new("http://127.0.0.1:1", "m", "e", Duration::from_millis(200)).unwrap();
        let e = o.recommend("sys", "user").await.unwrap_err();
        assert!(matches!(e, Error::Transport(_)), "{e:?}");
        assert!(!o.ready().await.0);
    }

    /// A model name with a tag must match a pulled model with the same base name,
    /// so `nemotron-3-nano:4b` is satisfied by `nemotron-3-nano:4b`.
    #[test]
    fn model_presence_matches_on_the_base_name() {
        let have = ["nemotron-3-nano:4b".to_string(), "nomic-embed-text:latest".to_string()];
        let present = |want: &str| {
            have.iter()
                .any(|h| h == want || h.split(':').next() == want.split(':').next())
        };
        assert!(present("nemotron-3-nano:4b"));
        assert!(present("nomic-embed-text"));
        assert!(!present("llama3"));
    }

    /// The embeddings endpoint does not promise ordering, and a reordered list
    /// silently pairs every tab with the wrong vector.
    #[test]
    fn embeddings_are_restored_to_request_order() {
        let body = r#"{"data":[
            {"index":2,"embedding":[3.0]},
            {"index":0,"embedding":[1.0]},
            {"index":1,"embedding":[2.0]}]}"#;
        let parsed: OpenAiEmbeddings = serde_json::from_str(body).unwrap();
        let mut rows: Vec<(usize, Vec<f32>)> =
            parsed.data.into_iter().map(|d| (d.index, d.embedding)).collect();
        rows.sort_by_key(|(i, _)| *i);
        let vectors: Vec<Vec<f32>> = rows.into_iter().map(|(_, v)| v).collect();
        assert_eq!(vectors, vec![vec![1.0], vec![2.0], vec![3.0]]);
    }

    #[test]
    fn an_ollama_reply_is_read_out_of_its_envelope() {
        let body = r#"{"message":{"role":"assistant","content":"{\"summary\":\"ok\"}"},"done":true}"#;
        let parsed: OllamaChat = serde_json::from_str(body).unwrap();
        let rec = prompt::extract_json(&parsed.message.content).unwrap();
        assert_eq!(rec.summary, "ok");
    }

    #[test]
    fn an_openai_reply_is_read_out_of_its_envelope() {
        let body = r#"{"choices":[{"index":0,"message":{"role":"assistant",
                       "content":"{\"summary\":\"ok\"}"},"finish_reason":"stop"}]}"#;
        let parsed: OpenAiChat = serde_json::from_str(body).unwrap();
        let rec = prompt::extract_json(&parsed.choices[0].message.content).unwrap();
        assert_eq!(rec.summary, "ok");
    }

    #[test]
    fn a_reply_with_no_choices_is_an_error_rather_than_an_empty_recommendation() {
        let parsed: OpenAiChat = serde_json::from_str(r#"{"choices":[]}"#).unwrap();
        let content = parsed.choices.first().map(|c| c.message.content.as_str()).unwrap_or("");
        assert!(prompt::extract_json(content).is_err());
    }
}
