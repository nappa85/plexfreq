#ifndef BACKEND_H
#define BACKEND_H
#include <QObject>
#include <QVariantMap>
#include <QSet>
#include <QAbstractListModel>
#include "plexfreq_core.h"

class EntryModel : public QAbstractListModel {
    Q_OBJECT
    Q_PROPERTY(QVariantList entries READ entries WRITE replace NOTIFY entriesChanged)
public:
    explicit EntryModel(QObject *parent=nullptr):QAbstractListModel(parent) {}
    int rowCount(const QModelIndex &parent=QModelIndex()) const override {return parent.isValid()?0:m_entries.size();}
    QVariant data(const QModelIndex &index,int role) const override;
    QHash<int,QByteArray> roleNames() const override {return {{Qt::UserRole+1,"entry"},{Qt::UserRole+2,"albumGroup"},{Qt::UserRole+3,"groupTitle"}};}
    void replace(const QVariantList &entries);
    void append(const QVariantList &entries);
    QVariantList entries() const {return m_entries;}
signals:
    void entriesChanged();
private: QVariantList m_entries;
};

class Backend : public QObject {
    Q_OBJECT
    friend class BackendTest;
    Q_PROPERTY(QVariantMap state READ state NOTIFY stateChanged)
    Q_PROPERTY(QVariantMap cache READ cache NOTIFY cacheChanged)
    // Large collections never live in `state`/`cache` (Qt 5.6 converts a whole
    // QVariantMap/QVariantList on every QML read). See docs/architecture.md.
    Q_PROPERTY(QVariantList items READ items NOTIFY itemsChanged)
    Q_PROPERTY(QVariantList queueItems READ queueItems NOTIFY queueItemsChanged)
    Q_PROPERTY(int itemCount READ itemCount NOTIFY itemsChanged)
    Q_PROPERTY(int queueCount READ queueCount NOTIFY queueItemsChanged)
    Q_PROPERTY(int cacheRevision READ cacheRevision NOTIFY cacheRevisionChanged)
    Q_PROPERTY(bool busy READ busy NOTIFY busyChanged)
    Q_PROPERTY(bool loadingMore READ loadingMore NOTIFY loadingMoreChanged)
    Q_PROPERTY(QAbstractItemModel *itemsModel READ itemsModel CONSTANT)
    Q_PROPERTY(QAbstractItemModel *queueModel READ queueModel CONSTANT)
    Q_PROPERTY(QString error READ error NOTIFY errorChanged)
    Q_PROPERTY(bool playing READ playing NOTIFY playbackChanged)
    Q_PROPERTY(bool buffering READ buffering NOTIFY playbackChanged)
    Q_PROPERTY(qint64 position READ position NOTIFY playbackChanged)
    Q_PROPERTY(qint64 duration READ duration NOTIFY playbackChanged)
public:
    explicit Backend(const QString &directory,QObject *parent=nullptr);
    ~Backend() override;
    QVariantMap state() const {return m_state;}
    QVariantMap cache() const {return m_cache;}
    QVariantList items() const {return m_items.entries();}
    QVariantList queueItems() const {return m_queue.entries();}
    int itemCount() const {return m_items.rowCount();}
    int queueCount() const {return m_queue.rowCount();}
    int cacheRevision() const {return m_cacheRevision;}
    // Cache key sets. Bindings reference `cacheRevision` for dependency.
    Q_INVOKABLE bool isReady(const QString &key) const {return m_ready.contains(key);}
    Q_INVOKABLE bool isGone(const QString &key) const {return m_gone.contains(key);}
    Q_INVOKABLE bool isPinned(const QString &key) const {return m_pinned.contains(key);}
    Q_INVOKABLE bool isPinnedGroup(const QString &key) const {return m_pinnedGroups.contains(key);}
    bool busy() const {return m_busy;}
    bool loadingMore() const {return m_more;}
    QAbstractItemModel *itemsModel(){return &m_items;}
    QAbstractItemModel *queueModel(){return &m_queue;}
    QString error() const {return m_error;}
    bool playing() const {return m_playback.value("playing").toBool();}
    bool buffering() const {return m_playback.value("buffering").toBool();}
    Q_PROPERTY(bool paused READ paused NOTIFY playbackChanged)
    bool paused() const {return m_playback.value("paused").toBool();}
    Q_PROPERTY(double volume READ volume NOTIFY playbackChanged)
    double volume() const {return m_playback.value("volume",0.8).toDouble();}
    Q_PROPERTY(bool seekable READ seekable NOTIFY playbackChanged)
    bool seekable() const {return m_playback.value("seekable").toBool();}
    qint64 position() const {return m_playback.value("position").toLongLong();}
    qint64 duration() const {return m_playback.value("duration").toLongLong();}
    Q_INVOKABLE void command(const QString &op,const QVariantMap &args=QVariantMap());
    Q_INVOKABLE void play(){command("audio_play");}
    Q_INVOKABLE void pause(){command("audio_pause");}
    Q_INVOKABLE void stop(){command("audio_stop");}
    Q_INVOKABLE void togglePlayback(){command("audio_toggle");}
    Q_INVOKABLE void seek(qint64 position){command("audio_seek",{{"position",position}});}
    Q_INVOKABLE void setVolume(double volume){command("audio_volume",{{"volume",volume}});}
signals:
    void stateChanged();void cacheChanged();void cacheRevisionChanged();void itemsChanged();void queueItemsChanged();void busyChanged();void loadingMoreChanged();void errorChanged();void playbackChanged();
    void seeked(qint64 position);void raiseRequested();
    void completed(const QString &op,bool ok,const QVariantMap &data);
private:
    void poll();void handle(const QVariantMap &event);void applyCache(QVariantMap cache);void status(const QVariantMap &value);
    void reportError(const QString &text,bool radio=false);
    Runtime *m_runtime=nullptr;
public:
    // A new artwork source handle for the image provider (caller owns it).
    ArtworkSource *artworkSource() const {return m_runtime ? pf_runtime_artwork(m_runtime) : nullptr;}
private:
    QVariantMap m_state,m_cache,m_playback;
    QSet<QString> m_ready,m_gone,m_pinned,m_pinnedGroups;
    int m_cacheRevision=0;
    QString m_error;bool m_busy=false,m_more=false;
    EntryModel m_items,m_queue;
};
#endif
