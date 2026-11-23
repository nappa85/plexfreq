//! Helpers shared by integration test crates (`mod support;`).

/// Plex downloads through the backend-neutral scheduling API.
pub fn plex_schedule(
    cache: &plexfreq_core::cache::Cache,
    server: &str,
    token: &str,
    items: Vec<plexfreq_core::model::Item>,
) -> plexfreq_core::Result<()> {
    plex_schedule_at(cache, server, token, items, 0)
}

/// [`plex_schedule`] at a download quality (0: the original file).
pub fn plex_schedule_at(
    cache: &plexfreq_core::cache::Cache,
    server: &str,
    token: &str,
    items: Vec<plexfreq_core::model::Item>,
    quality_kbps: u32,
) -> plexfreq_core::Result<()> {
    let policy =
        plexfreq_core::server::StreamPolicy::plex(&plexfreq_core::plex::server_url(server)?, token);
    let namespace = plexfreq_core::cache::namespace(server, token);
    cache.schedule_for(&namespace, policy, items, quality_kbps)
}
