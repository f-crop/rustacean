mod admin;

#[allow(unused_imports)]
pub use admin::{ApplyResult, KafkaAdmin, TopicDef, TopicStatus, TopicsFile, load_topics_file};

use std::path::Path;

use anyhow::Result;

pub async fn apply_topics(bootstrap_servers: &str, config_path: &Path) -> Result<ApplyResult> {
    let topics_file = load_topics_file(config_path)?;
    let kafka = KafkaAdmin::new(bootstrap_servers)?;
    kafka.apply(&topics_file).await
}

pub fn print_status(bootstrap_servers: &str, config_path: &Path) -> Result<()> {
    let topics_file = load_topics_file(config_path)?;
    let kafka = KafkaAdmin::new(bootstrap_servers)?;
    let statuses = kafka.status(&topics_file)?;

    println!("{:<40} {:>8} {:>8} STATUS", "TOPIC", "DESIRED", "LIVE");
    println!("{}", "-".repeat(65));
    for s in &statuses {
        let live = s
            .live_partitions
            .map_or_else(|| "-".to_owned(), |n| n.to_string());
        let status = if s.exists { "ok" } else { "missing" };
        println!(
            "{:<40} {:>8} {:>8} {}",
            s.name, s.desired_partitions, live, status
        );
    }
    Ok(())
}
