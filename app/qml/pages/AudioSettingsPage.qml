import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    property var music
    property var config:backend.state.audioConfig || ({crossfadeMs:0,normalization:false,eq:[0,0,0,0,0,0,0,0,0,0]})
    property var quality:backend.state.qualityConfig || ({wifiKbps:0,mobileKbps:0,downloadKbps:0,codecFallback:true})
    SilicaFlickable {anchors.fill:parent;contentHeight:body.height
        Column {id:body;width:parent.width;spacing:Theme.paddingMedium
            PageHeader {title:qsTr("Audio settings")}
            SectionHeader {text:qsTr("Streaming quality")}
            Repeater {model:[{key:"wifiKbps",title:qsTr("Wi-Fi audio")},{key:"mobileKbps",title:qsTr("Mobile audio")},{key:"downloadKbps",title:qsTr("Download audio")}]
                ComboBox {property string settingKey:modelData.key;width:body.width;label:modelData.title;value:music.qualityText(quality[settingKey])
                    menu:ContextMenu {Repeater {model:[0,64,96,128,160,192,256,320];MenuItem {text:music.qualityText(modelData);onClicked:music.qualitySetting(settingKey,modelData)}}}
                }
            }
            TextSwitch {text:qsTr("Plex codec fallback");checked:quality.codecFallback!==false;onClicked:music.qualitySetting("codecFallback",checked)}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:qsTr("Quality changes apply to following tracks and downloads. Completed local audio is preferred.");wrapMode:Text.Wrap;color:Theme.secondaryColor;font.pixelSize:Theme.fontSizeExtraSmall}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:qsTr("Rust audio engine · gapless PCM output");wrapMode:Text.Wrap;color:Theme.highlightColor}
            ComboBox {width:parent.width;label:qsTr("Crossfade");value:config.crossfadeMs ? qsTr("%1 seconds").arg(config.crossfadeMs/1000) : qsTr("Off")
                menu:ContextMenu {Repeater {model:[0,1000,3000,5000,8000,12000];MenuItem {text:modelData ? qsTr("%1 seconds").arg(modelData/1000) : qsTr("Off");onClicked:music.audioSetting("crossfadeMs",modelData)}}}
            }
            TextSwitch {text:qsTr("Normalize loudness");checked:!!config.normalization;onClicked:music.audioSetting("normalization",checked)}
            ComboBox {width:parent.width;label:qsTr("Normalization mode");value:config.normalizationMode==="album" ? qsTr("Album gain") : config.normalizationMode==="auto" ? qsTr("Automatic gain") : qsTr("Track gain")
                menu:ContextMenu {MenuItem {text:qsTr("Track gain");onClicked:music.audioSetting("normalizationMode","track")}MenuItem {text:qsTr("Album gain");onClicked:music.audioSetting("normalizationMode","album")}MenuItem {text:qsTr("Automatic gain");onClicked:music.audioSetting("normalizationMode","auto")}}
            }
            Slider {width:parent.width;minimumValue:0;maximumValue:12;stepSize:1;value:config.headroomDb || 0;label:qsTr("Headroom");valueText:qsTr("%1 dB").arg(value);onReleased:music.audioSetting("headroomDb",value)}
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;text:qsTr("Uses Plex gain metadata when available. Tracks within the same album keep gapless transitions without crossfade.");wrapMode:Text.Wrap;color:Theme.secondaryColor;font.pixelSize:Theme.fontSizeExtraSmall}
            SectionHeader {text:qsTr("Equalizer")}
            Repeater {model:[31,62,125,250,500,1000,2000,4000,8000,16000]
                Slider {width:body.width;minimumValue:-12;maximumValue:12;stepSize:1;value:config.eq ? config.eq[index] : 0;label:modelData+" Hz";valueText:value+" dB";onReleased:{var gains=config.eq.slice();gains[index]=value;music.audioSetting("eq",gains)}}
            }
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:qsTr("Reset equalizer");onClicked:music.audioSetting("eq",[0,0,0,0,0,0,0,0,0,0])}
        }
        VerticalScrollDecorator {}
    }
}
