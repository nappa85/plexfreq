#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
image="${SDK_IMAGE:-plexfreq-audio-sdk:local}"
target="${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}"
stage="$root/build/device-tests"
library="$root/target/aarch64-unknown-linux-gnu/release/libplexfreq_core.a"
if [[ ! -f "$library" ]]; then
    printf 'Run tools/build-sailfish.sh first to build the Rust static library.\n' >&2
    exit 1
fi
mkdir -p "$stage/thirdparty" "$stage/rpm" "$stage/tests" "$stage/app/qml/shared" "$stage/app/qml/desktop" "$stage/app/qml/pages"
cp "$root/tests/qt-backend-test.qrc" "$stage/tests/"
cp "$root/tests/HeaderGeometry.qml" "$stage/tests/"
cp "$root/tests/RadioIconGeometry.qml" "$stage/tests/"
cp "$root/tests/PageNavigation.qml" "$stage/tests/"
cp "$root/app/qml/shared/Session.qml" "$root/app/qml/shared/RadioIcon.qml" "$stage/app/qml/shared/"
cp "$root/app/qml/shared/Navigation.qml" "$stage/app/qml/shared/"
cp "$root/app/qml/shared/TextLink.qml" "$stage/app/qml/shared/"
cp "$root/app/qml/desktop/Main.qml" "$root/app/qml/desktop/NavigationMenu.qml" "$stage/app/qml/desktop/"
cp "$root/app/qml/shared/IntrinsicLoader.qml" "$stage/app/qml/shared/"
cp "$root/app/qml/shared/BrowsePageView.qml" "$stage/app/qml/shared/"
cp "$root/app/qml/shared/AlphabetRail.qml" "$stage/app/qml/shared/"
cp "$root/app/qml/desktop/RadioAction.qml" "$root/app/qml/desktop/DetailHeader.qml" "$stage/app/qml/desktop/"
cp "$root/app/qml/desktop/NowPlaying.qml" "$stage/app/qml/desktop/"
cp "$root/app/qml/desktop/PlaylistEditor.qml" "$root/app/qml/desktop/Downloads.qml" "$root/app/qml/desktop/AudioSettings.qml" "$stage/app/qml/desktop/"
cp "$root/app/qml/desktop/Filters.qml" "$root/app/qml/desktop/DiscoverySettings.qml" "$root/app/qml/desktop/Insights.qml" "$stage/app/qml/desktop/"
cp "$root/app/qml/pages/NowPlayingPage.qml" "$stage/app/qml/pages/"
cp "$root/app/qml/pages/PlaylistDialog.qml" "$root/app/qml/pages/DownloadsPage.qml" "$root/app/qml/pages/AudioSettingsPage.qml" "$stage/app/qml/pages/"
cp "$root/app/qml/pages/LibraryPage.qml" "$root/app/qml/pages/ArtistPage.qml" "$root/app/qml/pages/AlbumPage.qml" "$root/app/qml/pages/DetailHeader.qml" "$root/app/qml/pages/RadioAction.qml" "$stage/app/qml/pages/"
cp "$root/app/qml/pages/FiltersDialog.qml" "$root/app/qml/pages/DiscoverySettingsPage.qml" "$root/app/qml/pages/InsightsPage.qml" "$stage/app/qml/pages/"
cp "$root/tests/qt_backend.cpp" "$stage/"
cp "$root/app/src/backend.cpp" "$root/app/src/backend.h" "$root/app/src/plexfreq_core.h" "$stage/"
cp "$root/app/src/i18n.cpp" "$root/app/src/i18n.h" "$stage/"
cp "$root/app/translations.qrc" "$stage/app/"
cp -a "$root/app/translations" "$stage/app/"
cp "$root/tests/sailfish/plexfreq-backend-test.pro" "$stage/"
cp "$root/tests/sailfish/rpm/plexfreq-backend-test.spec" "$stage/rpm/"
cp "$library" "$stage/thirdparty/"
owner="$(id -u):$(id -g)"
restore_owner() {
    local result=$?
    docker run --rm --user root -v "$stage:/home/mersdk/app" \
        --entrypoint chown "$image" -R "$owner" /home/mersdk/app || { local cleanup=$?; (( result != 0 )) || result=$cleanup; }
    exit "$result"
}
trap restore_owner EXIT
docker run --rm --user root -v "$stage:/home/mersdk/app" \
    --entrypoint chown "$image" -R mersdk:mersdk /home/mersdk/app
docker run --rm -v "$stage:/home/mersdk/app" -w /home/mersdk/app \
    --entrypoint mb2 "$image" --target "$target" build
printf 'Device fixture: %s/plexfreq-backend-test\n' "$stage"
