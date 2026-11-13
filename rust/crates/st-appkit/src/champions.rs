//! The champion list from Riot's Data Dragon (Glossary.refresh_champions), cached for 3 days.

use anyhow::{Context, Result};
use serde_json::Value;
use st_core::glossary::{Champion, Glossary, DDRAGON};

fn get(url: &str) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(10))).build().into();
    Ok(agent.get(url).header("User-Agent", "StreamTranslator").call()?.body_mut().read_json()?)
}

/// Returns true if a new list was saved.
pub fn refresh(gl: &mut Glossary, force: bool) -> Result<bool> {
    if !force && gl.version.is_some() && crate::now() - gl.updated < 3.0 * 86400.0 {
        return Ok(false);
    }
    let version = get(&format!("{DDRAGON}/api/versions.json"))?[0].as_str().context("bad Data Dragon answer")?.to_string();
    let data = get(&format!("{DDRAGON}/cdn/{version}/data/en_US/champion.json"))?;
    let mut champs: Vec<Champion> = data["data"]
        .as_object()
        .context("bad Data Dragon answer")?
        .values()
        .map(|c| Champion { name: c["name"].as_str().unwrap_or("").to_string(), id: c["id"].as_str().map(String::from) })
        .collect();
    champs.sort_by(|a, b| a.name.cmp(&b.name));
    gl.set_champions(champs, version, crate::now())?;
    Ok(true)
}
