import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    id:dialog
    property var music
    title:music.editorAction==="add" ? qsTr("Add to playlist") : music.editorAction==="rename" ? qsTr("Rename playlist") : qsTr("Create playlist")
    modal:true;standardButtons:Dialog.Save|Dialog.Cancel
    function validate(){var save=standardButton(Dialog.Save);if(save)save.enabled=!backend.busy && (music.editorAction==="add" ? choices.currentIndex>=0 && choices.model.length>0 && !choices.model[choices.currentIndex].smart : name.text.trim().length>0 && (music.editorAction==="rename" || music.editorItems.length>0))}
    onOpened:{name.text=music.editorAction==="rename" ? music.editorTarget.title : "";validate()}
    onAccepted:music.submitPlaylist(music.editorAction==="rename" ? music.editorTarget.ratingKey : music.editorAction==="add" ? choices.model[choices.currentIndex].ratingKey : "",name.text)
    Connections {target:backend;onStateChanged:dialog.validate();onBusyChanged:dialog.validate()}
    contentItem:ColumnLayout {
        TextField {id:name;Layout.fillWidth:true;visible:music.editorAction!=="add";placeholderText:qsTr("Playlist name");onTextChanged:dialog.validate()}
        ComboBox {id:choices;Layout.fillWidth:true;visible:music.editorAction==="add";model:backend.state.playlistChoices || [];textRole:"title";onCurrentIndexChanged:dialog.validate()}
        Label {Layout.fillWidth:true;text:qsTr("Smart playlists use Plex filters. New playlists use selected tracks/album, or the queue.");wrapMode:Text.Wrap}
    }
}
