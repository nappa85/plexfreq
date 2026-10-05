import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3

Dialog {
    property var music
    property var config:backend.state.audioConfig || ({crossfadeMs:0,normalization:false,eq:[0,0,0,0,0,0,0,0,0,0]})
    property var quality:backend.state.qualityConfig || ({wifiKbps:0,mobileKbps:0,downloadKbps:0,codecFallback:true})
    title:qsTr("Audio settings");standardButtons:Dialog.Close
    contentItem:ScrollView {implicitWidth:500;implicitHeight:560;clip:true
    ColumnLayout {width:parent.width
        Label {text:qsTr("Streaming quality");font.pixelSize:20}
        Repeater {model:[{key:"wifiKbps",title:qsTr("Wi-Fi audio")},{key:"mobileKbps",title:qsTr("Mobile audio")},{key:"downloadKbps",title:qsTr("Download audio")}]
            ComboBox {property string settingKey:modelData.key;property string settingTitle:modelData.title;Layout.fillWidth:true;model:[0,64,96,128,160,192,256,320];displayText:settingTitle+": "+music.qualityText(quality[settingKey]);onActivated:music.qualitySetting(settingKey,model[currentIndex])}
        }
        Switch {text:qsTr("Plex codec fallback");checked:quality.codecFallback!==false;onClicked:music.qualitySetting("codecFallback",checked)}
        Label {Layout.fillWidth:true;text:qsTr("Quality changes apply to following tracks and downloads. Completed local audio is preferred.");wrapMode:Text.Wrap}
        Label {Layout.fillWidth:true;text:qsTr("Rust audio engine · gapless PCM output");wrapMode:Text.Wrap}
        ComboBox {Layout.fillWidth:true;model:[0,1000,3000,5000,8000,12000];displayText:qsTr("Crossfade")+": "+(config.crossfadeMs ? qsTr("%1 seconds").arg(config.crossfadeMs/1000) : qsTr("Off"));onActivated:music.audioSetting("crossfadeMs",model[currentIndex])}
        Switch {text:qsTr("Normalize loudness");checked:!!config.normalization;onClicked:music.audioSetting("normalization",checked)}
        ComboBox {Layout.fillWidth:true;model:[qsTr("Track gain"),qsTr("Album gain"),qsTr("Automatic gain")];currentIndex:config.normalizationMode==="album" ? 1 : config.normalizationMode==="auto" ? 2 : 0;onActivated:music.audioSetting("normalizationMode",["track","album","auto"][currentIndex])}
        Label {text:qsTr("Headroom")+": "+qsTr("%1 dB").arg(config.headroomDb || 0)}
        Slider {Layout.fillWidth:true;from:0;to:12;stepSize:1;value:config.headroomDb || 0;onPressedChanged:if(!pressed)music.audioSetting("headroomDb",value)}
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
}
