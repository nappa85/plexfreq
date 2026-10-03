#ifndef PLEXFREQ_CORE_H
#define PLEXFREQ_CORE_H
// Stable opaque C ABI; each handle is exclusively owned by one worker thread.
#ifdef __cplusplus
extern "C" {
#endif
typedef struct Core Core;
typedef struct DownloadControl DownloadControl;
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
