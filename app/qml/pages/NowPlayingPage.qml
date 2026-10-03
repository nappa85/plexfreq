import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    id:page
    property var music
    property var track: backend.state.track || ({})
    allowedOrientations:Orientation.All
    onStatusChanged: if(status===PageStatus.Active)music.nowPlaying=true
    Component.onDestruction:music.nowPlaying=false
    SilicaFlickable {
        anchors.fill:parent;contentHeight:content.height
        PullDownMenu {MenuItem{text:qsTr("Reload lyrics");enabled:!!music.trackKey;onClicked:music.requestLyrics()}}
        Column {
            id:content;width:parent.width;spacing:Theme.paddingMedium
            PageHeader {title:qsTr("Now playing")}
            Image {anchors.horizontalCenter:parent.horizontalCenter;width:Math.min(parent.width-2*Theme.horizontalPageMargin,page.height*0.4);height:width;source:track.artwork || "";fillMode:Image.PreserveAspectFit;asynchronous:true}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:track.title || qsTr("Nothing playing");font.pixelSize:Theme.fontSizeLarge;color:Theme.highlightColor;wrapMode:Text.Wrap;horizontalAlignment:Text.AlignHCenter}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:music.artist(track)+"\n"+(track.parentTitle || "");wrapMode:Text.Wrap;horizontalAlignment:Text.AlignHCenter;color:Theme.secondaryColor}
            Row {anchors.horizontalCenter:parent.horizontalCenter;spacing:Theme.paddingLarge
                IconButton {icon.source:"image://theme/icon-m-previous";enabled:!backend.busy;onClicked:backend.position>3000?backend.seek(0):backend.command("previous")}
                IconButton {icon.source:backend.playing?"image://theme/icon-m-pause":"image://theme/icon-m-play";onClicked:backend.togglePlayback()}
                IconButton {icon.source:"image://theme/icon-m-next";enabled:!backend.busy;onClicked:backend.command("next")}
            }
            Slider {width:parent.width;minimumValue:0;maximumValue:Math.max(1,backend.duration);value:backend.position;valueText:music.time(backend.position)+" / "+music.time(backend.duration);onReleased:backend.seek(value)}
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:track.userRating>=10?qsTr("★ Favorite"):qsTr("☆ Add to favorites");enabled:!backend.busy && !!music.trackKey;onClicked:music.favorite(track)}
            SectionHeader {text:qsTr("Lyrics")}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;visible:music.lyrics.length===0;text:music.lyricsState==="loading"?qsTr("Loading lyrics…"):music.lyricsState==="unavailable"?qsTr("Lyrics unavailable while offline or on this server."):qsTr("No lyrics provided by Plex.");wrapMode:Text.Wrap;color:Theme.secondaryColor}
            Repeater {model:music.lyrics
                Label {x:Theme.horizontalPageMargin;width:content.width-2*x;text:modelData.text;textFormat:Text.PlainText;wrapMode:Text.Wrap;horizontalAlignment:Text.AlignHCenter;color:index===music.lyricIndex()?Theme.highlightColor:Theme.primaryColor}
            }
            Item {width:1;height:Theme.paddingLarge}
        }
        VerticalScrollDecorator {}
    }
}
