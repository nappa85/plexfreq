import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    property var music
    property var config:backend.state.audioConfig || ({crossfadeMs:0,normalization:false,eq:[0,0,0,0,0,0,0,0,0,0]})
    title:qsTr("Audio settings");standardButtons:Dialog.Close
    contentItem:ColumnLayout {
        Label {Layout.fillWidth:true;text:qsTr("Rust audio engine · gapless PCM output");wrapMode:Text.Wrap}
        ComboBox {Layout.fillWidth:true;model:[0,1000,3000,5000,8000,12000];displayText:qsTr("Crossfade")+": "+(config.crossfadeMs ? qsTr("%1 seconds").arg(config.crossfadeMs/1000) : qsTr("Off"));onActivated:music.audioSetting("crossfadeMs",model[currentIndex])}
        Switch {text:qsTr("Normalize loudness");checked:!!config.normalization;onClicked:music.audioSetting("normalization",checked)}
        Label {Layout.fillWidth:true;text:qsTr("Uses Plex gain metadata when available. Tracks within the same album keep gapless transitions without crossfade.");wrapMode:Text.Wrap}
        Label {text:qsTr("Equalizer");font.pixelSize:20}
        Repeater {model:[31,62,125,250,500,1000,2000,4000,8000,16000]
            RowLayout {Layout.fillWidth:true
                Label {text:modelData+" Hz";Layout.preferredWidth:70}
                Slider {Layout.fillWidth:true;from:-12;to:12;stepSize:1;value:config.eq ? config.eq[index] : 0;onPressedChanged:if(!pressed){var gains=config.eq.slice();gains[index]=value;music.audioSetting("eq",gains)}}
                Label {text:(config.eq ? config.eq[index] : 0)+" dB";Layout.preferredWidth:50}
            }
        }
        Button {text:qsTr("Reset equalizer");onClicked:music.audioSetting("eq",[0,0,0,0,0,0,0,0,0,0])}
    }
}
