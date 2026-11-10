use mango_core::Result;
use mango_core::config::Config;

pub fn run(config: &Config) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(config)?);
    Ok(())
}
