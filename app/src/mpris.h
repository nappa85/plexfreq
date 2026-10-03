#ifndef PLEXFREQ_MPRIS_H
#define PLEXFREQ_MPRIS_H
#include "backend.h"
#include <QDBusAbstractAdaptor>
#include <QDBusObjectPath>

class MprisRoot : public QDBusAbstractAdaptor {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.mpris.MediaPlayer2")
    Q_PROPERTY(bool CanQuit READ no CONSTANT)
    Q_PROPERTY(bool CanRaise READ yes CONSTANT)
    Q_PROPERTY(bool HasTrackList READ no CONSTANT)
    Q_PROPERTY(QString Identity READ identity CONSTANT)
    Q_PROPERTY(QString DesktopEntry READ desktopEntry CONSTANT)
    Q_PROPERTY(QStringList SupportedUriSchemes READ empty CONSTANT)
    Q_PROPERTY(QStringList SupportedMimeTypes READ empty CONSTANT)
public:
    MprisRoot(QObject *parent, Backend *backend):QDBusAbstractAdaptor(parent),b(backend) {}
    bool no() const {return false;} bool yes() const {return true;}
    QString identity() const {return "PlexFreq";}
    QString desktopEntry() const {return "harbour-plexfreq";}
    QStringList empty() const {return {};}
public slots:
    void Raise() {emit b->raiseRequested();}
    void Quit() {}
private: Backend *b;
};

class MprisPlayer : public QDBusAbstractAdaptor {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.mpris.MediaPlayer2.Player")
    Q_PROPERTY(QString PlaybackStatus READ status)
    Q_PROPERTY(QString LoopStatus READ loop WRITE setLoop)
    Q_PROPERTY(double Rate READ rate WRITE setRate)
    Q_PROPERTY(bool Shuffle READ shuffle WRITE setShuffle)
    Q_PROPERTY(QVariantMap Metadata READ metadata)
    Q_PROPERTY(double Volume READ volume WRITE setVolume)
    Q_PROPERTY(qlonglong Position READ position)
    Q_PROPERTY(double MinimumRate READ rate CONSTANT)
    Q_PROPERTY(double MaximumRate READ rate CONSTANT)
    Q_PROPERTY(bool CanGoNext READ canSkip)
    Q_PROPERTY(bool CanGoPrevious READ canSkip)
    Q_PROPERTY(bool CanPlay READ canPlay)
    Q_PROPERTY(bool CanPause READ canPlay)
    Q_PROPERTY(bool CanSeek READ canSeek)
    Q_PROPERTY(bool CanControl READ yes CONSTANT)
public:
    MprisPlayer(QObject *parent,Backend *backend);
    QString status() const;
    QString loop() const;
    void setLoop(const QString &value);
    double rate() const {return 1.;} void setRate(double value) {if(value==0)Pause();}
    bool shuffle() const {return b->state().value("queue").toMap().value("shuffled").toBool();}
    void setShuffle(bool value) {if(!b->busy() && b->state().value("radio").toMap().isEmpty())b->command("shuffle",{{"enabled",value}});}
    QVariantMap metadata() const;
    double volume() const {return b->volume();} void setVolume(double value) {b->setVolume(value);}
    qlonglong position() const {return b->position()*1000;}
    bool canSkip() const {return !b->busy() && canPlay();}
    bool canPlay() const {return !b->state().value("queue").toMap().value("items").toList().isEmpty();}
    bool canSeek() const {return b->seekable();}
    bool yes() const {return true;}
    QDBusObjectPath trackId() const;
    void notify();
public slots:
    void Next() {if(canSkip()) b->command("next",{{"automatic",false},{"_paused",!b->playing()}});}
    void Previous() {if(canSkip()) b->command("previous",{{"_paused",!b->playing()}});}
    void Pause() {b->pause();}
    void PlayPause() {b->togglePlayback();}
    void Stop() {b->stop();}
    void Play() {b->play();}
    void Seek(qlonglong offset);
    void SetPosition(const QDBusObjectPath &id,qlonglong value);
    void OpenUri(const QString &) {}
signals:
    void Seeked(qlonglong position);
private:
    Backend *b;
    QVariantMap last;
};

class MprisService : public QObject {
public:
    explicit MprisService(Backend *backend);
    ~MprisService() override;
private: bool registered=false;
};
#endif
