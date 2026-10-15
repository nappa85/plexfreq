//! The typed request contract: QML/FFI JSON is parsed into `Command` once, at
//! runtime admission. Routing classification lives here as exhaustive matches
//! so a new operation cannot be added without deciding how the worker treats it.
use crate::daily::{DownloadKind, GroupId, INVALID_GROUP};
use crate::queue::Repeat;
use crate::{
    audio, filters,
    model::{Item, ItemKind},
    quality, Error, RadioKind, Result,
};
use serde::{Deserialize, Serialize};

/// Library collection shortcuts (`collection.view`).
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum CollectionView {
    Favorites,
    Added,
    Played,
}

/// `download_action.action` on a pinned download group.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum DownloadAction {
    Refresh,
    Retry,
    /// Unpin, keeping already cached audio.
    Cancel,
    /// Unpin and delete cached audio no other pin needs.
    Remove,
}

/// `playlist_edit.action` on a regular playlist.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PlaylistAction {
    Create,
    Rename,
    Delete,
    Add,
    Remove,
    Move,
}

/// `smart_playlist.action`.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SmartAction {
    Create,
    Update,
}

#[derive(Deserialize, strum::IntoStaticStr, strum::VariantNames)]
#[serde(tag = "op", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Command {
    // Transport controls act on the audio engine at admission (GUI thread);
    // they never reach Core.
    AudioPause,
    AudioStop,
    AudioSeek {
        position: u64,
    },
    AudioVolume {
        volume: f64,
    },
    AudioPlay,
    AudioToggle,
    AudioConfig {
        config: audio::dsp::Config,
    },
    Status,
    RefreshPolicy {
        enabled: bool,
        interval_hours: u32,
    },
    RefreshDownloads,
    SmartRules {
        key: String,
    },
    DiscoveryConfig {
        section: String,
        hidden: Vec<String>,
        order: Vec<String>,
    },
    HubItems {
        key: String,
        #[serde(default)]
        start: usize,
    },
    ListeningInsights {
        days: u32,
    },
    SonicJourney {
        section: String,
        keys: Vec<String>,
    },
    FilterOptions {
        section: String,
        kind: ItemKind,
        field: String,
    },
    FilteredBrowse {
        section: String,
        kind: ItemKind,
        sort: String,
        filters: filters::Music,
        #[serde(default)]
        start: usize,
    },
    SmartPlaylist {
        action: SmartAction,
        #[serde(default)]
        key: String,
        section: String,
        #[serde(default)]
        title: String,
        sort: String,
        limit: usize,
        filters: filters::Music,
    },
    QualityConfig {
        config: quality::Config,
    },
    TranscodedPlayback,
    ProbeQuality {
        key: String,
        kbps: u32,
    },
    ProbePlayback {
        key: String,
        kbps: u32,
    },
    DownloadStation {
        key: String,
        title: String,
        minutes: u32,
    },
    Station {
        key: String,
        title: String,
    },
    DownloadPlan {
        kind: DownloadKind,
        key: String,
        #[serde(default)]
        minutes: u32,
    },
    PlayDownload {
        group: GroupId,
    },
    OfflineSearch {
        query: String,
        #[serde(default)]
        kind: ItemKind,
        #[serde(default)]
        start: usize,
    },
    DiscoveryHome {
        section: String,
    },
    SonicNeighbors {
        key: String,
        kind: ItemKind,
    },
    SonicAdventure {
        section: String,
        start_key: String,
        end_key: String,
    },
    PlaylistChoices,
    PlaylistEdit {
        action: PlaylistAction,
        #[serde(default)]
        key: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        items: Vec<Item>,
        #[serde(default)]
        item_id: Option<u64>,
        #[serde(default)]
        after: Option<u64>,
    },
    PlaylistItems {
        key: String,
        #[serde(default)]
        start: usize,
    },
    LibraryBrowse {
        section: String,
        kind: ItemKind,
        sort: String,
        #[serde(default)]
        start: usize,
    },
    ArtistAlbums {
        key: String,
    },
    Downloads,
    DownloadPolicy {
        wifi_only: bool,
        paused: bool,
    },
    DownloadAction {
        group: GroupId,
        action: DownloadAction,
    },
    NetworkState {
        wifi: bool,
        online: bool,
        #[serde(default)]
        live_hint: bool,
    },
    Autoplay {
        enabled: bool,
    },
    Mix {
        seeds: Vec<Item>,
    },
    PlaybackEvent {
        key: String,
        occurrence: String,
        listened: u64,
        duration: u64,
    },
    SyncHistory,
    OfflineMode {
        enabled: bool,
    },
    OfflineBrowse {
        kind: ItemKind,
    },
    CacheArtwork {
        items: Vec<Item>,
    },
    Pin {
        key: String,
        kind: ItemKind,
        enabled: bool,
    },
    Lyrics {
        key: String,
    },
    Search {
        query: String,
    },
    Collection {
        section: String,
        view: CollectionView,
        #[serde(default)]
        start: usize,
    },
    Favorite {
        key: String,
        enabled: bool,
    },
    Login,
    PollLogin,
    Servers,
    Connect {
        url: String,
        token: String,
    },
    SelectServer {
        index: usize,
    },
    Libraries,
    Browse {
        section: String,
        kind: ItemKind,
        #[serde(default)]
        query: String,
        #[serde(default)]
        start: usize,
    },
    Alphabet {
        section: String,
    },
    JumpArtist {
        section: String,
        letter: String,
    },
    Children {
        key: String,
        #[serde(default)]
        start: usize,
    },
    Detail {
        key: String,
        #[serde(default)]
        start: usize,
    },
    SimilarArtists {
        key: String,
    },
    Playlists {
        #[serde(default)]
        start: usize,
    },
    Play {
        items: Vec<Item>,
        index: usize,
    },
    Enqueue {
        items: Vec<Item>,
        #[serde(default)]
        next: bool,
    },
    EnqueueAlbum {
        key: String,
        #[serde(default)]
        next: bool,
    },
    PlayAlbum {
        key: String,
    },
    QueueRemove {
        index: usize,
    },
    QueueMove {
        from: usize,
        to: usize,
    },
    Resume,
    SavePlayback {
        key: String,
        position: u64,
        #[serde(default)]
        generation: Option<u64>,
    },
    Radio {
        key: String,
        kind: RadioKind,
    },
    StopRadio,
    Timeline {
        state: String,
        position: u64,
        duration: u64,
        #[serde(default)]
        continuing: bool,
    },
    CacheStatus,
    CacheConfig {
        enabled: bool,
        limit_mb: u64,
        ahead: usize,
    },
    CacheTracks {
        items: Vec<Item>,
    },
    CachedTracks,
    CachedPlayback,
    ClearCache,
    SelectTrack {
        index: usize,
    },
    Next {
        #[serde(default)]
        automatic: bool,
    },
    Previous,
    Shuffle {
        enabled: bool,
    },
    Repeat {
        mode: Repeat,
    },
    Logout,
}

/// How the runtime routes and post-processes one command.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Class {
    /// Replaces (or, with a start offset, appends to) the visible item list.
    pub listing: bool,
    /// Reply is correlated with the navigation generation that requested it.
    pub view_bound: bool,
    /// Never raises the busy indicator.
    pub passive: bool,
    /// Checkpoints the current audio occurrence before Core changes playback.
    pub transport: bool,
    /// The prepared successor must be rebuilt after success.
    pub reprepare: bool,
    /// Stops audio and clears the presented session (server switch/logout).
    pub resets_session: bool,
    /// Executed against the audio engine at admission; never queued.
    pub engine: bool,
}

impl Class {
    const PLAIN: Self = Self {
        listing: false,
        view_bound: false,
        passive: false,
        transport: false,
        reprepare: false,
        resets_session: false,
        engine: false,
    };
    const LISTING: Self = Self {
        listing: true,
        view_bound: true,
        ..Self::PLAIN
    };
    const PASSIVE: Self = Self {
        passive: true,
        ..Self::PLAIN
    };
    const TRANSPORT: Self = Self {
        transport: true,
        ..Self::PLAIN
    };
    const REPREPARE: Self = Self {
        reprepare: true,
        ..Self::PLAIN
    };
    const ENGINE: Self = Self {
        engine: true,
        ..Self::PLAIN
    };
}

impl Command {
    /// Wire name of the operation (the JSON `op`), for diagnostics.
    pub(crate) fn op(&self) -> &'static str {
        self.into()
    }

    /// Exhaustive routing classification: no wildcard arm, so every new
    /// variant must be classified explicitly.
    pub(crate) fn class(&self) -> Class {
        use Command as C;
        match self {
            C::Browse { .. }
            | C::FilteredBrowse { .. }
            | C::Collection { .. }
            | C::HubItems { .. }
            | C::SonicJourney { .. }
            | C::DiscoveryHome { .. }
            | C::SonicNeighbors { .. }
            | C::SonicAdventure { .. }
            | C::OfflineSearch { .. }
            | C::LibraryBrowse { .. }
            | C::Detail { .. }
            | C::ArtistAlbums { .. }
            | C::Children { .. }
            | C::PlaylistItems { .. }
            | C::Playlists { .. }
            | C::Search { .. }
            | C::OfflineBrowse { .. }
            | C::CachedTracks
            | C::JumpArtist { .. } => Class::LISTING,
            C::SimilarArtists { .. } => Class {
                view_bound: true,
                ..Class::PASSIVE
            },
            C::CacheStatus
            | C::CacheArtwork { .. }
            | C::Lyrics { .. }
            | C::Alphabet { .. }
            | C::Timeline { .. }
            | C::SavePlayback { .. }
            | C::PlaybackEvent { .. }
            | C::SyncHistory
            | C::PlaylistChoices
            | C::FilterOptions { .. }
            | C::SmartRules { .. }
            | C::NetworkState { .. }
            | C::RefreshDownloads => Class::PASSIVE,
            C::Play { .. }
            | C::PlayDownload { .. }
            | C::Station { .. }
            | C::PlayAlbum { .. }
            | C::SelectTrack { .. }
            | C::Next { .. }
            | C::Previous
            | C::Radio { .. }
            | C::Mix { .. }
            | C::StopRadio
            | C::ClearCache => Class::TRANSPORT,
            C::QueueRemove { .. } => Class {
                reprepare: true,
                ..Class::TRANSPORT
            },
            C::Connect { .. } | C::SelectServer { .. } | C::Logout => Class {
                resets_session: true,
                ..Class::TRANSPORT
            },
            C::Enqueue { .. }
            | C::EnqueueAlbum { .. }
            | C::QueueMove { .. }
            | C::Shuffle { .. }
            | C::Repeat { .. }
            | C::QualityConfig { .. }
            | C::AudioConfig { .. } => Class::REPREPARE,
            C::AudioPause
            | C::AudioStop
            | C::AudioSeek { .. }
            | C::AudioVolume { .. }
            | C::AudioPlay
            | C::AudioToggle => Class::ENGINE,
            C::Status
            | C::RefreshPolicy { .. }
            | C::DiscoveryConfig { .. }
            | C::ListeningInsights { .. }
            | C::SmartPlaylist { .. }
            | C::TranscodedPlayback
            | C::ProbeQuality { .. }
            | C::ProbePlayback { .. }
            | C::DownloadStation { .. }
            | C::DownloadPlan { .. }
            | C::PlaylistEdit { .. }
            | C::Downloads
            | C::DownloadPolicy { .. }
            | C::DownloadAction { .. }
            | C::Autoplay { .. }
            | C::OfflineMode { .. }
            | C::Pin { .. }
            | C::Favorite { .. }
            | C::Login
            | C::PollLogin
            | C::Servers
            | C::Libraries
            | C::Resume
            | C::CacheConfig { .. }
            | C::CacheTracks { .. }
            | C::CachedPlayback => Class::PLAIN,
        }
    }

    /// Requested page offset of a paged listing (zero for everything else).
    pub(crate) fn start(&self) -> usize {
        use Command as C;
        match self {
            C::HubItems { start, .. }
            | C::FilteredBrowse { start, .. }
            | C::OfflineSearch { start, .. }
            | C::PlaylistItems { start, .. }
            | C::LibraryBrowse { start, .. }
            | C::Collection { start, .. }
            | C::Browse { start, .. }
            | C::Children { start, .. }
            | C::Detail { start, .. }
            | C::Playlists { start } => *start,
            _ => 0,
        }
    }

    /// Parse one wire request. Internal worker coordination is not a Command,
    /// so `_`-prefixed and unknown ops fail here, before admission.
    pub(crate) fn parse(request: &serde_json::Value) -> Result<Self> {
        Self::deserialize(request).map_err(|error| {
            // Keep the specific (translated) messages for malformed transport
            // arguments and unknown enum values; anything else is generic
            // invalid request data.
            let unknown = error.to_string().contains("unknown variant");
            if error.to_string().contains(INVALID_GROUP) {
                return Error::Input("Download group no longer exists");
            }
            match request["op"].as_str() {
                Some("audio_seek") => Error::Input("Invalid seek position"),
                Some("audio_volume") => Error::Input("Invalid audio volume"),
                Some("repeat") if unknown => Error::Input("Unknown repeat mode"),
                Some("collection") if unknown => Error::Input("Unknown discovery view"),
                Some("download_action") if unknown => Error::Input("Unknown download action"),
                Some("download_plan") if unknown => {
                    Error::Input("Choose a playlist, album or radio download")
                }
                Some("playlist_edit" | "smart_playlist") if unknown => {
                    Error::Input("Unknown playlist action")
                }
                _ => Error::from(error),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use strum::VariantNames;

    fn agree<T>()
    where
        T: strum::IntoEnumIterator
            + std::fmt::Display
            + std::str::FromStr
            + Serialize
            + serde::de::DeserializeOwned
            + PartialEq
            + std::fmt::Debug,
        <T as std::str::FromStr>::Err: std::fmt::Debug,
    {
        for value in T::iter() {
            let name = value.to_string();
            assert_eq!(serde_json::to_value(&value).unwrap(), json!(name));
            assert_eq!(serde_json::from_value::<T>(json!(name)).unwrap(), value);
            assert_eq!(name.parse::<T>().unwrap(), value);
        }
    }

    #[test]
    fn strum_and_serde_spellings_agree() {
        agree::<CollectionView>();
        agree::<DownloadAction>();
        agree::<PlaylistAction>();
        agree::<SmartAction>();
        agree::<Repeat>();
        agree::<RadioKind>();
        agree::<DownloadKind>();
        agree::<crate::PlaybackSource>();
    }

    #[test]
    fn strum_op_names_match_serde_tags() {
        // Every strum name must be a serde tag: an unknown variant fails with
        // "unknown variant", a known one at worst with a missing field.
        for op in Command::VARIANTS {
            let error = match Command::deserialize(&json!({"op": op})) {
                Ok(command) => {
                    assert_eq!(command.op(), *op);
                    continue;
                }
                Err(error) => error.to_string(),
            };
            assert!(!error.contains("unknown variant"), "{op}: {error}");
        }
        let seek = Command::parse(&json!({"op":"audio_seek","position":5})).unwrap();
        assert_eq!(seek.op(), "audio_seek");
        assert_eq!(seek.class(), Class::ENGINE);
        for op in ["_shutdown", "_audio_checkpoint", "_update", "nonsense"] {
            assert!(Command::parse(&json!({"op": op})).is_err());
        }
        for (request, message) in [
            (
                json!({"op":"repeat","mode":"sometimes"}),
                "Unknown repeat mode",
            ),
            (json!({"op":"repeat"}), "Invalid request data"),
            (
                json!({"op":"collection","section":"1","view":"recent"}),
                "Unknown discovery view",
            ),
            (
                json!({"op":"download_action","group":"album:1","action":"pause"}),
                "Unknown download action",
            ),
            (
                json!({"op":"playlist_edit","action":"clear"}),
                "Unknown playlist action",
            ),
            (
                json!({"op":"play_download","group":"nonsense"}),
                "Download group no longer exists",
            ),
            (
                json!({"op":"download_plan","kind":"genre_radio","key":"1"}),
                "Choose a playlist, album or radio download",
            ),
        ] {
            assert_eq!(
                Command::parse(&request).err().map(|e| e.to_string()),
                Some(message.into())
            );
        }
        assert_eq!(
            Command::parse(&json!({"op":"audio_seek","position":-1}))
                .err()
                .map(|e| e.to_string()),
            Some("Invalid seek position".into())
        );
    }
}
