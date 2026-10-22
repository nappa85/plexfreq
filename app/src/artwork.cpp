#include "artwork.h"
#include <QAtomicInt>
#include <QBuffer>
#include <QImage>
#include <QImageReader>
#include <QMutex>
#include <QMutexLocker>
#include <QRunnable>
#include <QThreadPool>
#include <memory>

namespace {
// Process-lifetime pool: a provider destroyed at shutdown never waits for a
// stalled download (each running task owns its own source handle).
QThreadPool *pool() {
    static QThreadPool *instance=[]{auto *created=new QThreadPool;created->setMaxThreadCount(4);return created;}();
    return instance;
}

// The output size for an image of `original`: the requested size (keeping
// the aspect ratio) or the default limit, never larger than the original.
QSize target(const QSize &original,const QSize &requested) {
    QSize bound(ArtworkProvider::DefaultLimit,ArtworkProvider::DefaultLimit);
    if(requested.width()>0 || requested.height()>0)
        bound=QSize(requested.width()>0 ? requested.width() : ArtworkProvider::MaximumSide,requested.height()>0 ? requested.height() : ArtworkProvider::MaximumSide);
    bound=bound.boundedTo(QSize(ArtworkProvider::MaximumSide,ArtworkProvider::MaximumSide));
    if(original.width()<=bound.width() && original.height()<=bound.height())return original;
    // Never 0 on a side, even for extreme aspect ratios.
    return original.scaled(bound,Qt::KeepAspectRatio).expandedTo(QSize(1,1));
}

// Decode with the dimensions checked first: a small compressed file can
// describe a huge bitmap.
QImage decode(const QByteArray &bytes,const QSize &requested) {
    QBuffer buffer;buffer.setData(bytes);buffer.open(QIODevice::ReadOnly);
    QImageReader reader(&buffer);
    const QSize original=reader.size();
    if(!original.isValid() || original.width()>ArtworkProvider::SourceLimit || original.height()>ArtworkProvider::SourceLimit
        || qint64(original.width())*original.height()>ArtworkProvider::PixelLimit)return QImage();
    const QSize size=target(original,requested);
    if(size!=original)reader.setScaledSize(size);
    return reader.read();
}

QAtomicInt completed;

class ArtworkResponse;
// Shared by the pool task and the engine's response: either may go first.
struct ArtworkJob {
    QMutex mutex;
    ArtworkResponse *response=nullptr;
    QImage image;
    QAtomicInt cancelled;
};

class ArtworkResponse : public QQuickImageResponse {
public:
    explicit ArtworkResponse(std::shared_ptr<ArtworkJob> job):m_job(job) {m_job->response=this;}
    // The engine may delete a response while its download still runs: only
    // unlink it; the task finishes on its own.
    ~ArtworkResponse() override {QMutexLocker lock(&m_job->mutex);m_job->response=nullptr;}
    QQuickTextureFactory *textureFactory() const override {QMutexLocker lock(&m_job->mutex);return QQuickTextureFactory::textureFactoryForImage(m_job->image);}
    QString errorString() const override {QMutexLocker lock(&m_job->mutex);return m_job->image.isNull() ? QStringLiteral("Artwork unavailable") : QString();}
    void cancel() override {m_job->cancelled.storeRelease(1);}
private:
    std::shared_ptr<ArtworkJob> m_job;
};

class ArtworkTask : public QRunnable {
public:
    ArtworkTask(std::shared_ptr<ArtworkJob> job,ArtworkSource *source,const QString &id,const QSize &requested)
        :m_job(job),m_source(source),m_id(id.toUtf8()),m_requested(requested) {}
    ~ArtworkTask() override {pf_artwork_free(m_source);completed.fetchAndAddRelease(1);}
    void run() override {
        QImage image;
        if(!m_job->cancelled.loadAcquire()) {
            size_t length=0;
            unsigned char *bytes=pf_artwork_fetch(m_source,m_id.constData(),&length);
            if(bytes){image=decode(QByteArray(reinterpret_cast<const char *>(bytes),int(length)),m_requested);pf_bytes_free(bytes,length);}
        }
        QMutexLocker lock(&m_job->mutex);
        m_job->image=image;
        // Under the lock: the response cannot be deleted meanwhile. The
        // engine's image reader lives in its own thread, so `finished` reaches
        // it queued: no slot runs (or deletes the response) inside this lock.
        if(m_job->response)emit m_job->response->finished();
    }
private:
    std::shared_ptr<ArtworkJob> m_job;
    ArtworkSource *m_source;
    QByteArray m_id;
    QSize m_requested;
};
}

ArtworkProvider::ArtworkProvider(ArtworkSource *source):m_source(source) {}
int ArtworkProvider::completedTasks() {return completed.loadAcquire();}
ArtworkProvider::~ArtworkProvider() {pf_artwork_free(m_source);}
QQuickImageResponse *ArtworkProvider::requestImageResponse(const QString &id,const QSize &requestedSize) {
    auto job=std::make_shared<ArtworkJob>();
    auto *response=new ArtworkResponse(job);
    pool()->start(new ArtworkTask(job,pf_artwork_clone(m_source),id,requestedSize));
    return response;
}
