import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    id:dialog
    property var music
    property var track:backend.state.track || ({})
    title:qsTr("Now playing");modal:true;standardButtons:Dialog.Close
    onOpened:music.nowPlaying=true
    onClosed:music.nowPlaying=false
    contentItem:ScrollView {
        id:scroll;implicitHeight:Math.min(600,body.implicitHeight);contentWidth:availableWidth;clip:true
        ColumnLayout {id:body;width:scroll.availableWidth;spacing:12
            Image {Layout.alignment:Qt.AlignHCenter;Layout.preferredWidth:Math.min(body.width,260);Layout.preferredHeight:width;source:track.artwork || "";fillMode:Image.PreserveAspectFit;asynchronous:true}
            Label {Layout.fillWidth:true;text:track.title || qsTr("Nothing playing");font.pixelSize:24;wrapMode:Text.Wrap;horizontalAlignment:Text.AlignHCenter}
            Label {Layout.fillWidth:true;text:music.artist(track)+" · "+(track.parentTitle || "");wrapMode:Text.Wrap;horizontalAlignment:Text.AlignHCenter}
            RowLayout {Layout.alignment:Qt.AlignHCenter
                Button {text:"⏮";enabled:!backend.busy;onClicked:backend.position>3000?backend.seek(0):backend.command("previous")}
                Button {text:backend.playing?"⏸":"▶";onClicked:backend.togglePlayback()}
                Button {text:"⏭";enabled:!backend.busy;onClicked:backend.command("next")}
            }
            Slider {id:progress;objectName:"playbackSlider";Layout.fillWidth:true;from:0;to:Math.max(1,backend.duration);value:backend.position;onMoved:backend.seek(value)
                Connections {target:backend;onPlaybackChanged:if(!progress.pressed)progress.value=backend.position}
            }
            Label {text:music.time(backend.position)+" / "+music.time(backend.duration);Layout.alignment:Qt.AlignHCenter}
            Button {text:track.userRating>=10?qsTr("★ Favorite"):qsTr("☆ Add to favorites");enabled:!backend.busy && !!music.trackKey;Layout.alignment:Qt.AlignHCenter;onClicked:music.favorite(track)}
            Label {text:qsTr("Lyrics");font.pixelSize:20}
            Label {Layout.fillWidth:true;visible:music.lyrics.length===0;text:music.lyricsState==="loading"?qsTr("Loading lyrics…"):music.lyricsState==="unavailable"?qsTr("Lyrics unavailable while offline or on this server."):qsTr("No lyrics provided by Plex.");wrapMode:Text.Wrap}
            Repeater {model:music.lyrics
                Label {Layout.fillWidth:true;text:modelData.text;textFormat:Text.PlainText;wrapMode:Text.Wrap;horizontalAlignment:Text.AlignHCenter;color:index===music.lyricIndex()?"#ebad3d":"#edf0f5"}
            }
            Button {text:qsTr("Reload lyrics");enabled:!!music.trackKey;onClicked:music.requestLyrics()}
        }
    }
}
