#pragma once

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32) && defined(OWO_LIBIME_BRIDGE_EXPORTS)
#define OWO_LIBIME_BRIDGE_API __declspec(dllexport)
#elif defined(_WIN32)
#define OWO_LIBIME_BRIDGE_API __declspec(dllimport)
#else
#define OWO_LIBIME_BRIDGE_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

enum { OWO_LIBIME_BRIDGE_ABI_VERSION = 1 };

typedef void* owo_libime_handle;

OWO_LIBIME_BRIDGE_API uint32_t owo_libime_abi_version(void);
OWO_LIBIME_BRIDGE_API owo_libime_handle owo_libime_open(const char* model_path,
                                                         char* diagnostic,
                                                         size_t diagnostic_size);
OWO_LIBIME_BRIDGE_API int owo_libime_score(owo_libime_handle handle,
                                           const char* context,
                                           const char* candidate,
                                           float* score,
                                           char* diagnostic,
                                           size_t diagnostic_size);
OWO_LIBIME_BRIDGE_API int owo_libime_score_batch(owo_libime_handle handle,
                                                 const char* context,
                                                 const char* const* candidates,
                                                 size_t candidate_count,
                                                 float* scores,
                                                 char* diagnostic,
                                                 size_t diagnostic_size);
OWO_LIBIME_BRIDGE_API void owo_libime_close(owo_libime_handle handle);

#ifdef __cplusplus
}
#endif
