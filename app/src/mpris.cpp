#include "mpris.h"
#include <QDBusConnection>
#include <QDBusMessage>

MprisPlayer::MprisPlayer(QObject *parent,Backend *backend):QDBusAbstractAdaptor(parent),b(backend) {
    connect(b,&Backend::stateChanged,this,&MprisPlayer::notify);
    connect(b,&Backend::playbackChanged,this,&MprisPlayer::notify);
    connect(b,&Backend::busyChanged,this,&MprisPlayer::notify);
    connect(b,&Backend::seeked,this,[this](qint64 value){emit Seeked(value*1000);});
}
QString MprisPlayer::status() const {return b->playing() ? "Playing" : b->paused() ? "Paused" : "Stopped";}
QString MprisPlayer::loop() const {
    const auto mode=b->state().value("queue").toMap().value("repeat").toString();
    return mode=="one" ? "Track" : mode=="all" ? "Playlist" : "None";
}
void MprisPlayer::setLoop(const QString &value) {
    if(!b->busy() && b->state().value("radio").toMap().isEmpty() && (value=="Track" || value=="Playlist" || value=="None"))
        b->command("repeat",{{"mode",value=="Track" ? "one" : value=="Playlist" ? "all" : "off"}});
}
QDBusObjectPath MprisPlayer::trackId() const {
    const auto key=b->state().value("track").toMap().value("ratingKey").toString();
    if(key.isEmpty())return QDBusObjectPath("/org/mpris/MediaPlayer2/TrackList/NoTrack");
    return QDBusObjectPath("/org/plexfreq/track/"+key+"_"+b->state().value("playbackGeneration").toString()+"_"+b->state().value("queue").toMap().value("current").toString());
}
QVariantMap MprisPlayer::metadata() const {
    const auto track=b->state().value("track").toMap(); if(track.isEmpty())return {};
    QVariantMap result{{"mpris:trackid",QVariant::fromValue(trackId())},{"mpris:length",track.value("duration").toLongLong()*1000},
        {"xesam:title",track.value("title")},{"xesam:album",track.value("parentTitle")},
        {"xesam:artist",QStringList{track.value("originalTitle").toString().isEmpty()?track.value("grandparentTitle").toString():track.value("originalTitle").toString()}}};
    // Only local artwork is exported: never publish authenticated framework URLs.
    const auto art=track.value("artwork").toString(); if(QUrl(art).isLocalFile())result.insert("mpris:artUrl",art);
    return result;
}
void MprisPlayer::notify() {
    const QVariantMap values{{"PlaybackStatus",status()},{"LoopStatus",loop()},{"Shuffle",shuffle()},{"Metadata",metadata()},
        {"Volume",volume()},{"CanPlay",canPlay()},{"CanPause",canPlay()},{"CanSeek",canSeek()},{"CanGoNext",canSkip()},{"CanGoPrevious",canSkip()}};
    QVariantMap changed; for(auto i=values.constBegin();i!=values.constEnd();++i)if(last.value(i.key())!=i.value())changed.insert(i.key(),i.value());
    last=values;if(changed.isEmpty())return;
    auto message=QDBusMessage::createSignal("/org/mpris/MediaPlayer2","org.freedesktop.DBus.Properties","PropertiesChanged");
    message<<QString("org.mpris.MediaPlayer2.Player")<<changed<<QStringList();QDBusConnection::sessionBus().send(message);
}
void MprisPlayer::Seek(qlonglong offset) {
    if(!canSeek())return;
    const qlonglong value=position()+offset;
    if(value>b->duration()*1000)Next();else b->seek(qMax<qlonglong>(0,value)/1000);
}
void MprisPlayer::SetPosition(const QDBusObjectPath &id,qlonglong value) {
    if(canSeek() && id.path()==trackId().path() && value>=0 && value<=b->duration()*1000)b->seek(value/1000);
}
MprisService::MprisService(Backend *backend):QObject(backend) {
    auto bus=QDBusConnection::sessionBus();
    if(!bus.registerService("org.mpris.MediaPlayer2.plexfreq"))return;
    new MprisRoot(this,backend);new MprisPlayer(this,backend);
    registered=bus.registerObject("/org/mpris/MediaPlayer2",this,QDBusConnection::ExportAdaptors);
    if(!registered)bus.unregisterService("org.mpris.MediaPlayer2.plexfreq");
}
MprisService::~MprisService() {
    if(registered){auto bus=QDBusConnection::sessionBus();bus.unregisterObject("/org/mpris/MediaPlayer2");bus.unregisterService("org.mpris.MediaPlayer2.plexfreq");}
}
