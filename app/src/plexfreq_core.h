#ifndef PLEXFREQ_CORE_H
#define PLEXFREQ_CORE_H
#include <stddef.h>
#include <stdbool.h>
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
void pf_runtime_notify(Runtime *runtime,bool radio,const char *text);
void pf_runtime_free(Runtime *runtime);
// Artwork for image://plexfreq ids. The source stays valid after
// pf_runtime_free (requests then answer null); free it once with
// pf_artwork_free. Requests outlive it.
ArtworkSource *pf_runtime_artwork(Runtime *runtime);
void pf_artwork_free(ArtworkSource *source);
typedef struct ArtworkRequest ArtworkRequest;
// Called once, possibly before pf_artwork_request returns, from Rust's runtime
// thread (or whichever thread drops the request's task at process teardown):
// the image bytes (valid only during the call) or null.
typedef void (*ArtworkDone)(void *context,const unsigned char *bytes,size_t length);
ArtworkRequest *pf_artwork_request(const ArtworkSource *source,const char *id,ArtworkDone done,void *context);
// After it returns `done` never runs (a running one is waited for); any
// thread, repeatable.
void pf_artwork_cancel(const ArtworkRequest *request);
// Cancels and frees; once, with no pf_artwork_cancel running.
void pf_artwork_release(ArtworkRequest *request);
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
