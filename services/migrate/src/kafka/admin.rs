#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rdkafka::admin::{
    AdminClient, AdminOptions, AlterConfig, NewTopic, OwnedResourceSpecifier, ResourceSpecifier,
    TopicReplication,
};
use rdkafka::client::DefaultClientContext;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{BaseConsumer, Consumer};
use rdkafka::error::RDKafkaErrorCode;
use rdkafka::metadata::MetadataTopic;
use serde::Deserialize;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
pub struct TopicsFile {
    pub topics: Vec<TopicDef>,
}

#[derive(Debug, Deserialize)]
pub struct TopicDef {
    pub name: String,
    pub partitions: i32,
    pub replication_factor: i32,
    #[serde(default)]
    pub config: HashMap<String, String>,
}

#[derive(Debug)]
pub struct TopicStatus {
    pub name: String,
    pub exists: bool,
    pub live_partitions: Option<i32>,
    pub desired_partitions: i32,
}

#[derive(Debug, Default)]
pub struct ApplyResult {
    pub created: usize,
    pub configs_applied: usize,
    pub skipped: usize,
}

pub struct KafkaAdmin {
    admin: AdminClient<DefaultClientContext>,
    bootstrap_servers: String,
    opts: AdminOptions,
}

impl KafkaAdmin {
    pub fn new(bootstrap_servers: &str) -> Result<Self> {
        let admin = ClientConfig::new()
            .set("bootstrap.servers", bootstrap_servers)
            .create::<AdminClient<DefaultClientContext>>()
            .context("creating Kafka admin client")?;

        let opts = AdminOptions::new().operation_timeout(Some(Duration::from_secs(30)));

        Ok(Self {
            admin,
            bootstrap_servers: bootstrap_servers.to_owned(),
            opts,
        })
    }

    fn fetch_existing_topics(&self) -> Result<HashMap<String, i32>> {
        let consumer: BaseConsumer = ClientConfig::new()
            .set("bootstrap.servers", &self.bootstrap_servers)
            .set("group.id", "migrate-kafka-meta")
            .create()
            .context("creating metadata consumer")?;

        let metadata = consumer
            .fetch_metadata(None, Duration::from_secs(10))
            .context("fetching Kafka metadata")?;

        Ok(metadata
            .topics()
            .iter()
            .filter(|t: &&MetadataTopic| !t.name().starts_with("__"))
            .map(|t: &MetadataTopic| {
                (
                    t.name().to_owned(),
                    i32::try_from(t.partitions().len()).unwrap_or(0),
                )
            })
            .collect())
    }

    pub async fn apply(&self, topics_file: &TopicsFile) -> Result<ApplyResult> {
        let existing = self.fetch_existing_topics()?;

        let to_create: Vec<&TopicDef> = topics_file
            .topics
            .iter()
            .filter(|t| !existing.contains_key(&t.name))
            .collect();

        let created = self.create_topics(&to_create).await?;

        let with_config: Vec<&TopicDef> = topics_file
            .topics
            .iter()
            .filter(|t| !t.config.is_empty())
            .collect();

        let configs_applied = self.alter_topic_configs(&with_config).await?;

        let skipped = topics_file
            .topics
            .iter()
            .filter(|t| existing.contains_key(&t.name) && t.config.is_empty())
            .count();

        Ok(ApplyResult {
            created,
            configs_applied,
            skipped,
        })
    }

    async fn create_topics(&self, topics: &[&TopicDef]) -> Result<usize> {
        if topics.is_empty() {
            return Ok(0);
        }

        let new_topics: Vec<NewTopic<'_>> = topics
            .iter()
            .map(|t| {
                NewTopic::new(
                    &t.name,
                    t.partitions,
                    TopicReplication::Fixed(t.replication_factor),
                )
            })
            .collect();

        let results = self
            .admin
            .create_topics(&new_topics, &self.opts)
            .await
            .context("create_topics")?;

        let mut count = 0usize;
        for result in results {
            match result {
                Ok(name) => {
                    info!(topic = %name, "created");
                    count += 1;
                }
                Err((name, RDKafkaErrorCode::TopicAlreadyExists)) => {
                    info!(topic = %name, "already exists, skipped");
                }
                Err((name, code)) => {
                    anyhow::bail!("failed to create topic '{name}': {code}");
                }
            }
        }
        Ok(count)
    }

    async fn alter_topic_configs(&self, topics: &[&TopicDef]) -> Result<usize> {
        if topics.is_empty() {
            return Ok(0);
        }

        let alter_configs: Vec<AlterConfig<'_>> = topics
            .iter()
            .map(|t| {
                t.config.iter().fold(
                    AlterConfig::new(ResourceSpecifier::Topic(&t.name)),
                    |ac, (k, v)| ac.set(k, v),
                )
            })
            .collect();

        let results = self
            .admin
            .alter_configs(&alter_configs, &self.opts)
            .await
            .context("alter_configs")?;

        let mut count = 0usize;
        for result in results {
            match result {
                Ok(OwnedResourceSpecifier::Topic(name)) => {
                    info!(topic = %name, "config applied");
                    count += 1;
                }
                Ok(other) => {
                    warn!("unexpected resource in alter_configs result: {other:?}");
                }
                Err((spec, code)) => {
                    anyhow::bail!("failed to alter config for {spec:?}: {code}");
                }
            }
        }
        Ok(count)
    }

    pub fn status(&self, topics_file: &TopicsFile) -> Result<Vec<TopicStatus>> {
        let existing = self.fetch_existing_topics()?;

        Ok(topics_file
            .topics
            .iter()
            .map(|t| {
                let live_partitions = existing.get(&t.name).copied();
                TopicStatus {
                    name: t.name.clone(),
                    exists: live_partitions.is_some(),
                    live_partitions,
                    desired_partitions: t.partitions,
                }
            })
            .collect())
    }
}

pub fn load_topics_file(path: &Path) -> Result<TopicsFile> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_topics_file_valid() {
        let yaml = r#"
topics:
  - name: rb.ingest.clone.commands
    partitions: 6
    replication_factor: 1
    config:
      retention.ms: "604800000"
  - name: rb.audit.events
    partitions: 3
    replication_factor: 1
"#;
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(yaml.as_bytes()).unwrap();

        let tf = load_topics_file(f.path()).unwrap();
        assert_eq!(tf.topics.len(), 2);
        assert_eq!(tf.topics[0].name, "rb.ingest.clone.commands");
        assert_eq!(tf.topics[0].partitions, 6);
        assert_eq!(
            tf.topics[0].config.get("retention.ms").map(String::as_str),
            Some("604800000")
        );
        assert!(tf.topics[1].config.is_empty());
    }

    #[test]
    fn test_load_topics_file_missing() {
        let err = load_topics_file(std::path::Path::new("/nonexistent/topics.yaml")).unwrap_err();
        assert!(err.to_string().contains("reading"));
    }

    #[test]
    fn test_load_topics_file_invalid_yaml() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"not: valid: yaml: [[[").unwrap();
        let err = load_topics_file(f.path()).unwrap_err();
        assert!(err.to_string().contains("parsing"));
    }
}
