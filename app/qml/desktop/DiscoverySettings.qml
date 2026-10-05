import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    property var music
    title:qsTr("Customize discovery");standardButtons:Dialog.Close
    contentItem:ListView {implicitWidth:440;implicitHeight:360;clip:true;model:backend.state.discoveryHubs || [];ScrollBar.vertical:ScrollBar {}
        delegate:RowLayout {width:ListView.view.width
            Switch {Layout.fillWidth:true;text:modelData.title;checked:!modelData.hidden;enabled:!backend.busy;onClicked:music.toggleHub(modelData.identifier,!checked)}
            Button {text:"↑";enabled:index>0 && !backend.busy;onClicked:music.moveHub(index,-1)}
            Button {text:"↓";enabled:index+1<(backend.state.discoveryHubs || []).length && !backend.busy;onClicked:music.moveHub(index,1)}
        }
    }
}
