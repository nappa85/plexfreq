import QtQuick 2.6
import Sailfish.Silica 1.0
import "shared"
import "pages"

ApplicationWindow {
    id: app
    Session { id: session }
    Connections {
        target: session
        onNavigate: pageStack.push(Qt.resolvedUrl(kind === "artist" ? "pages/ArtistPage.qml" : "pages/AlbumPage.qml"), {music:session, pageKey:key})
        onOpenPlayer: pageStack.push(Qt.resolvedUrl("pages/NowPlayingPage.qml"),{music:session})
        onEditPlaylistRequested:pageStack.push(Qt.resolvedUrl("pages/PlaylistDialog.qml"),{music:session})
        onOpenDownloads:pageStack.push(Qt.resolvedUrl("pages/DownloadsPage.qml"),{music:session})
        onOpenAudioSettings:pageStack.push(Qt.resolvedUrl("pages/AudioSettingsPage.qml"),{music:session})
        onOpenFilterEditor:pageStack.push(Qt.resolvedUrl("pages/FiltersDialog.qml"),{music:session})
        onOpenDiscoverySettings:pageStack.push(Qt.resolvedUrl("pages/DiscoverySettingsPage.qml"),{music:session})
        onOpenInsights:pageStack.push(Qt.resolvedUrl("pages/InsightsPage.qml"),{music:session})
    }
    initialPage: Component { LibraryPage { music: session } }
    cover: Component {
        CoverBackground {
            id:playbackCover
            Column {
                anchors.top:parent.top;anchors.topMargin:Theme.paddingMedium;anchors.horizontalCenter:parent.horizontalCenter
                width: parent.width - 2 * Theme.paddingMedium;spacing:Theme.paddingSmall
                Item {
                    anchors.horizontalCenter:parent.horizontalCenter
                    width:Math.max(0,Math.min(parent.width,playbackCover.height-coverTitle.height-coverTrack.height-coverArtist.height-Theme.itemSizeSmall-2*Theme.paddingLarge));height:width;clip:true
                    Image {id:coverArt;anchors.fill:parent;source:backend.state.track ? backend.state.track.albumArtwork || backend.state.track.artwork || "" : "";asynchronous:true;fillMode:Image.PreserveAspectFit}
                    Label {anchors.centerIn:parent;visible:coverArt.status!==Image.Ready;text:"♪";font.pixelSize:Theme.fontSizeHuge;color:Theme.highlightColor}
                }
                Label {id:coverTitle;width:parent.width;text:"PlexFreq";color:Theme.secondaryColor;font.pixelSize:Theme.fontSizeExtraSmall;horizontalAlignment:Text.AlignHCenter}
                Label {id:coverTrack;width:parent.width;text:backend.state.track ? backend.state.track.title || qsTr("Nothing playing") : qsTr("Nothing playing");wrapMode:Text.Wrap;maximumLineCount:2;truncationMode:TruncationMode.Fade;horizontalAlignment:Text.AlignHCenter}
                Label {id:coverArtist;width:parent.width;text:backend.state.track ? session.artist(backend.state.track) : "";font.pixelSize:Theme.fontSizeExtraSmall;color:Theme.secondaryColor;truncationMode:TruncationMode.Fade;horizontalAlignment:Text.AlignHCenter}
            }
            CoverActionList {
                CoverAction { iconSource: backend.playing ? "image://theme/icon-cover-pause" : "image://theme/icon-cover-play"; onTriggered: backend.togglePlayback() }
                CoverAction { iconSource: "image://theme/icon-cover-next-song"; onTriggered: if (!backend.busy) backend.command("next", {automatic:false}) }
            }
        }
    }
}
