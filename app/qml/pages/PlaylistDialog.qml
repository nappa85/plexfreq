import QtQuick 2.6
import Sailfish.Silica 1.0

Dialog {
    property var music
    property string chosenKey:""
    property string chosenTitle:""
    canAccept:!backend.busy && (music.editorAction==="add" ? chosenKey.length>0 : name.text.trim().length>0 && (music.editorAction==="rename" || music.editorItems.length>0))
    onAccepted:music.submitPlaylist(music.editorAction==="rename" ? music.editorTarget.ratingKey : chosenKey,name.text)
    SilicaFlickable {anchors.fill:parent;contentHeight:body.height
        Column {id:body;width:parent.width;spacing:Theme.paddingMedium
            DialogHeader {title:music.editorAction==="add" ? qsTr("Add to playlist") : music.editorAction==="rename" ? qsTr("Rename playlist") : qsTr("Create playlist");acceptText:qsTr("Save")}
            TextField {id:name;width:parent.width;visible:music.editorAction!=="add";label:qsTr("Playlist name");text:music.editorAction==="rename" ? music.editorTarget.title : ""}
            ComboBox {width:parent.width;visible:music.editorAction==="add";label:qsTr("Playlist");value:chosenTitle
                menu:ContextMenu {Repeater {model:backend.state.playlistChoices || [];MenuItem {text:modelData.title;enabled:!modelData.smart;onClicked:{chosenKey=modelData.ratingKey;chosenTitle=modelData.title}}}}
            }
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:qsTr("Smart playlist contents are managed by Plex filters. New playlists use the selected tracks/album, or the queue.");wrapMode:Text.Wrap;color:Theme.secondaryColor;font.pixelSize:Theme.fontSizeExtraSmall}
        }
    }
}
