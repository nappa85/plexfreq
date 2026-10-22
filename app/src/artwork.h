#ifndef PLEXFREQ_ARTWORK_H
#define PLEXFREQ_ARTWORK_H
#include <QQuickImageProvider>
#include "plexfreq_core.h"

// Serves image://plexfreq ids. Rust loads the image (from its cache, or
// downloaded with the Plex credential, which never reaches QML); this only
// hands the bytes to Qt, which decodes them on its image reader thread.
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
private:
    ArtworkSource *m_source;
};
#endif
