#include "artwork.h"
#include <QAtomicInt>
#include <QBuffer>
#include <QImage>
#include <QImageReader>
#include <QMutex>
#include <QMutexLocker>

namespace {
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

class ArtworkResponse : public QQuickImageResponse {
public:
    explicit ArtworkResponse(const QSize &requested):m_requested(requested) {}
    // Rust waits for a running callback and never calls it afterwards.
    ~ArtworkResponse() override {pf_artwork_release(m_request);}
    // A request Rust could not create never calls back: finish here.
    void start(const ArtworkSource *source,const QString &id) {m_request=pf_artwork_request(source,id.toUtf8().constData(),&ArtworkResponse::done,this);if(!m_request)finish();}
    // Qt still expects `finished` after a cancel.
    void cancel() override {pf_artwork_cancel(m_request);finish();}
    QString errorString() const override {QMutexLocker lock(&m_mutex);return image().isNull() ? QStringLiteral("Artwork unavailable") : QString();}
    QQuickTextureFactory *textureFactory() const override {QMutexLocker lock(&m_mutex);return QQuickTextureFactory::textureFactoryForImage(image());}
private:
    // On the Rust runtime thread: keep the bytes, then let Qt know.
    static void done(void *context,const unsigned char *bytes,size_t length) {
        auto *self=static_cast<ArtworkResponse *>(context);
        if(bytes){QMutexLocker lock(&self->m_mutex);self->m_bytes=QByteArray(reinterpret_cast<const char *>(bytes),int(length));}
        self->finish();
    }
    // Once, queued: delivered on the response's thread after Qt connected
    // to it, whoever calls this.
    void finish() {if(m_finished.testAndSetOrdered(0,1))QMetaObject::invokeMethod(this,"finished",Qt::QueuedConnection);}
    // Decoded once, on Qt's image reader thread.
    const QImage &image() const {
        if(!m_decoded){m_image=decode(m_bytes,m_requested);m_bytes.clear();m_decoded=true;}
        return m_image;
    }
    const QSize m_requested;
    ArtworkRequest *m_request=nullptr;
    QAtomicInt m_finished;
    mutable QMutex m_mutex;
    mutable QByteArray m_bytes;
    mutable QImage m_image;
    mutable bool m_decoded=false;
};
}

ArtworkProvider::ArtworkProvider(ArtworkSource *source):m_source(source) {}
ArtworkProvider::~ArtworkProvider() {pf_artwork_free(m_source);}
QQuickImageResponse *ArtworkProvider::requestImageResponse(const QString &id,const QSize &requestedSize) {
    auto *response=new ArtworkResponse(requestedSize);
    response->start(m_source,id);
    return response;
}
