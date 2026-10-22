#ifndef PLEXFREQ_ARTWORK_H
#define PLEXFREQ_ARTWORK_H
#include <QQuickImageProvider>
#include "plexfreq_core.h"

// Serves image://plexfreq ids: Rust fetches the image with the Plex credential,
// which therefore never reaches QML. Each request runs on a small shared
// thread pool, so a slow image cannot hold up the others.
class ArtworkProvider : public QQuickAsyncImageProvider {
public:
    // Takes ownership of `source` (from pf_runtime_artwork).
    explicit ArtworkProvider(ArtworkSource *source);
    ~ArtworkProvider() override;
    QQuickImageResponse *requestImageResponse(const QString &id,const QSize &requestedSize) override;
    enum : int {
        // Longest side of an image shown without a requested size.
        DefaultLimit=1024,
        // Longest side ever produced, whatever the request.
        MaximumSide=2048,
        // Larger source images are refused before decoding: per side, and
        // in total pixels (formats without scaled decoding decode in full).
        SourceLimit=8192,
        PixelLimit=16*1024*1024
    };
    // Downloads finished so far (tests observe late completion).
    static int completedTasks();
private:
    ArtworkSource *m_source;
};
#endif
