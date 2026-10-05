import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    property var music
    property var stats:backend.state.insights || ({plays:0,listenedMs:0,artists:[],albums:[],tracks:[]})
    property int days:30
    property string category:"artists"
    title:qsTr("Listening insights");standardButtons:Dialog.Close
    onOpened:backend.command("listening_insights",{days:days})
    contentItem:ColumnLayout {
        Label {Layout.fillWidth:true;text:qsTr("%1 qualified plays · %2 listened").arg(stats.plays).arg(music.totalTime(stats.listenedMs));wrapMode:Text.Wrap}
        Label {Layout.fillWidth:true;text:qsTr("Based on locally measured qualified plays since insights tracking began.");wrapMode:Text.Wrap}
        ComboBox {Layout.fillWidth:true;model:[7,30,90,0];displayText:days ? qsTr("%1 days").arg(days) : qsTr("All history");onActivated:{days=model[currentIndex];backend.command("listening_insights",{days:days})}}
        ComboBox {Layout.fillWidth:true;model:[qsTr("Artists"),qsTr("Albums"),qsTr("Tracks")];onActivated:category=["artists","albums","tracks"][currentIndex]}
        ListView {Layout.fillWidth:true;Layout.preferredHeight:300;clip:true;model:stats[category] || [];ScrollBar.vertical:ScrollBar {}
            delegate:ItemDelegate {width:ListView.view.width;text:modelData.title+" · "+qsTr("%1 plays · %2 listened").arg(modelData.plays).arg(music.totalTime(modelData.listenedMs));enabled:false}
        }
    }
}
