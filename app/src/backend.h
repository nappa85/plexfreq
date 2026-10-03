#ifndef BACKEND_H
#define BACKEND_H
#include <QThread>
#include <QMutex>
#include <QWaitCondition>
#include <QQueue>
#include <QVariantMap>
#include <QMediaPlayer>
#include <QUrl>
#include <QAbstractListModel>
#include <QElapsedTimer>
struct DownloadControl;

class EntryModel : public QAbstractListModel {
    Q_OBJECT
public:
    explicit EntryModel(QObject *parent = nullptr) : QAbstractListModel(parent) {}
    int rowCount(const QModelIndex &parent = QModelIndex()) const override { return parent.isValid() ? 0 : m_entries.size(); }
    QVariant data(const QModelIndex &index, int role) const override { if(!index.isValid() || index.row()>=m_entries.size())return QVariant();return role==Qt::UserRole+1?m_entries.at(index.row()):role==Qt::UserRole+2?m_entries.at(index.row()).toMap().value("albumType"):QVariant(); }
    QHash<int,QByteArray> roleNames() const override { return {{Qt::UserRole + 1, "entry"},{Qt::UserRole+2,"albumGroup"}}; }
    void replace(const QVariantList &entries);
    void append(const QVariantList &entries);
private:
    QVariantList m_entries;
};

class CoreThread : public QThread {
    Q_OBJECT
public:
    explicit CoreThread(const QString &directory, QObject *parent = nullptr);
    ~CoreThread() override;
    void submit(const QByteArray &request);
    void networkHint(bool wifi);
    void policyHint(bool wifiOnly,bool paused);
signals:
    void result(const QByteArray &request, const QByteArray &response);
protected:
    void run() override;
private:
    QString m_directory;
    QMutex m_mutex;
    QWaitCondition m_wake;
    QQueue<QByteArray> m_requests;
    bool m_stopping = false;
    DownloadControl *m_downloadControl=nullptr;
    bool m_networkHintKnown=false,m_wifiHint=false,m_policyHintKnown=false,m_wifiPolicyHint=false,m_pausePolicyHint=false;
};

class Backend : public QObject {
    Q_OBJECT
    Q_PROPERTY(QVariantMap state READ state NOTIFY stateChanged)
    Q_PROPERTY(QVariantMap cache READ cache NOTIFY cacheChanged)
    Q_PROPERTY(bool busy READ busy NOTIFY busyChanged)
    Q_PROPERTY(bool loadingMore READ loadingMore NOTIFY loadingMoreChanged)
    Q_PROPERTY(QAbstractItemModel *itemsModel READ itemsModel CONSTANT)
    Q_PROPERTY(QAbstractItemModel *queueModel READ queueModel CONSTANT)
    Q_PROPERTY(QString error READ error NOTIFY errorChanged)
    Q_PROPERTY(bool playing READ playing NOTIFY playbackChanged)
    Q_PROPERTY(qint64 position READ position NOTIFY playbackChanged)
    Q_PROPERTY(qint64 duration READ duration NOTIFY playbackChanged)
public:
    explicit Backend(const QString &directory, QObject *parent = nullptr);
    QVariantMap state() const { return m_state; }
    QVariantMap cache() const { return m_cache; }
    bool busy() const { return m_busyPending > 0; }
    bool loadingMore() const { return m_morePending > 0; }
    QAbstractItemModel *itemsModel() { return &m_itemsModel; }
    QAbstractItemModel *queueModel() { return &m_queueModel; }
    QString error() const { return m_error; }
    bool playing() const;
    bool paused() const;
    double volume() const;
    qint64 position() const { return m_player->position(); }
    qint64 duration() const { return m_player->duration(); }
    Q_INVOKABLE void command(const QString &op, const QVariantMap &args = QVariantMap());
    Q_INVOKABLE void togglePlayback();
    Q_INVOKABLE void seek(qint64 position);
    Q_INVOKABLE void setVolume(double volume);
    Q_INVOKABLE void play();
    Q_INVOKABLE void pause();
    Q_INVOKABLE void stop();
    bool seekable() const {return m_player->isSeekable();}
signals:
    void stateChanged();
    void cacheChanged();
    void busyChanged();
    void loadingMoreChanged();
    void errorChanged();
    void playbackChanged();
    void seeked(qint64 position);
    void raiseRequested();
    void completed(const QString &op, bool ok, const QVariantMap &data);
private:
    void handle(const QByteArray &request, const QByteArray &response);
    void reportTimeline();
    void savePlayback();
    void samplePlayback();
    void recordPlayback();
    void sendTimeline(const QString &state,bool continuing=false);
private slots:
    void updateNetwork();
private:
    CoreThread *m_thread;
    QMediaPlayer *m_player;
    QVariantMap m_state;
    QVariantMap m_cache;
    QUrl m_source;
    qint64 m_restorePosition = -1;
    QString m_error;
    int m_pending = 0;
    int m_busyPending = 0;
    int m_morePending = 0;
    quint64 m_viewGeneration = 0;
    EntryModel m_itemsModel;
    EntryModel m_queueModel;
    qint64 m_listened=0,m_samplePosition=0;
    QElapsedTimer m_sampleClock;
    bool m_wasPlaying=false,m_switchingSource=false,m_stoppedReported=false,m_networkPending=false;
};
#endif
