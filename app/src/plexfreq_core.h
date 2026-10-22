#ifndef PLEXFREQ_CORE_H
#define PLEXFREQ_CORE_H
#include <stddef.h>
// Stable opaque C ABI; each handle is exclusively owned by one worker thread.
#ifdef __cplusplus
extern "C" {
#endif
typedef struct Core Core;
typedef struct Runtime Runtime;
typedef struct DownloadControl DownloadControl;
typedef struct DiagnosticLog DiagnosticLog;
typedef struct ArtworkSource ArtworkSource;
DiagnosticLog *pf_log_new(const char *directory);
void pf_log_free(DiagnosticLog *log);
Runtime *pf_runtime_new(const char *state_dir);
char *pf_runtime_submit(Runtime *runtime,const char *request);
char *pf_runtime_poll(Runtime *runtime);
void pf_runtime_free(Runtime *runtime);
// Artwork for image://plexfreq ids. Handles stay valid after pf_runtime_free
// (fetches then fail); each is freed once with pf_artwork_free.
ArtworkSource *pf_runtime_artwork(Runtime *runtime);
ArtworkSource *pf_artwork_clone(const ArtworkSource *source);
// Image bytes for an id, or null; blocks on the network (loader threads only).
unsigned char *pf_artwork_fetch(const ArtworkSource *source,const char *id,size_t *length);
void pf_bytes_free(unsigned char *bytes,size_t length);
void pf_artwork_free(ArtworkSource *source);
Core *pf_new(const char *state_dir);
// Diagnostic core: preserves cache configuration but never starts media downloads.
Core *pf_new_inspect(const char *state_dir);
// Clone on the Core thread; hints are thread-safe, with coordinated lifetime.
DownloadControl *pf_download_control(Core *core);
void pf_download_network(DownloadControl *control, int wifi);
void pf_download_policy(DownloadControl *control, int wifi_only, int paused);
void pf_download_control_free(DownloadControl *control);
char *pf_call(Core *core, const char *request);
void pf_free(Core *core);
void pf_string_free(char *value);
#ifdef __cplusplus
}
#endif
#endif
