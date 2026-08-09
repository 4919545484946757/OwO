#pragma once

#include "owo/config/config_store.h"

#include <Windows.h>
#include <msctf.h>

#include <condition_variable>
#include <cstdint>
#include <deque>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

struct ID2D1Factory;
struct ID2D1HwndRenderTarget;
struct ID2D1SolidColorBrush;
struct IDWriteFactory;
struct IDWriteTextFormat;

namespace owo::tsf {

inline constexpr CLSID kTextServiceClsid{
    0x6d31c9b1, 0x8978, 0x4f49, {0x89, 0xb4, 0x66, 0xeb, 0x1e, 0x74, 0x15, 0x91}};
inline constexpr GUID kLanguageProfileGuid{
    0x5d9f39c3, 0xbdb4, 0x453c, {0xa7, 0xba, 0xb9, 0xef, 0x82, 0x48, 0x76, 0x29}};
inline constexpr GUID kLanguageModePreservedKeyGuid{
    0x409a6b54, 0xd599, 0x4d7d, {0xa4, 0x36, 0xa1, 0x80, 0x60, 0xf5, 0xe1, 0x81}};
inline constexpr LANGID kSimplifiedChinese = 0x0804;

class TextService final : public ITfTextInputProcessorEx,
                          public ITfKeyEventSink,
                          public ITfThreadMgrEventSink,
                          public ITfThreadFocusSink {
public:
    TextService() noexcept;

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override;
    ULONG STDMETHODCALLTYPE AddRef() override;
    ULONG STDMETHODCALLTYPE Release() override;

    HRESULT STDMETHODCALLTYPE Activate(ITfThreadMgr* thread_manager, TfClientId client_id) override;
    HRESULT STDMETHODCALLTYPE ActivateEx(ITfThreadMgr* thread_manager,
                                         TfClientId client_id,
                                         DWORD flags) override;
    HRESULT STDMETHODCALLTYPE Deactivate() override;

    HRESULT STDMETHODCALLTYPE OnSetFocus(BOOL foreground) override;
    HRESULT STDMETHODCALLTYPE OnTestKeyDown(ITfContext* context,
                                            WPARAM key,
                                            LPARAM flags,
                                            BOOL* eaten) override;
    HRESULT STDMETHODCALLTYPE OnKeyDown(ITfContext* context,
                                        WPARAM key,
                                        LPARAM flags,
                                        BOOL* eaten) override;
    HRESULT STDMETHODCALLTYPE OnTestKeyUp(ITfContext* context,
                                          WPARAM key,
                                          LPARAM flags,
                                          BOOL* eaten) override;
    HRESULT STDMETHODCALLTYPE OnKeyUp(ITfContext* context,
                                      WPARAM key,
                                      LPARAM flags,
                                      BOOL* eaten) override;
    HRESULT STDMETHODCALLTYPE OnPreservedKey(ITfContext* context,
                                             REFGUID guid,
                                             BOOL* eaten) override;

    HRESULT STDMETHODCALLTYPE OnInitDocumentMgr(ITfDocumentMgr* document_manager) override;
    HRESULT STDMETHODCALLTYPE OnUninitDocumentMgr(ITfDocumentMgr* document_manager) override;
    HRESULT STDMETHODCALLTYPE OnSetFocus(ITfDocumentMgr* document_manager,
                                         ITfDocumentMgr* previous_document_manager) override;
    HRESULT STDMETHODCALLTYPE OnPushContext(ITfContext* context) override;
    HRESULT STDMETHODCALLTYPE OnPopContext(ITfContext* context) override;

    HRESULT STDMETHODCALLTYPE OnSetThreadFocus() override;
    HRESULT STDMETHODCALLTYPE OnKillThreadFocus() override;

private:
    struct CandidateResult {
        std::uint64_t request_id{};
        std::uint64_t generation{};
        std::uint64_t page{};
        bool has_more{};
        bool expanded{};
        std::uint64_t page_size{5};
        bool preserve_paging{};
        bool request_failed{};
        std::wstring failure_detail;
        std::wstring segmented_input;
        std::vector<std::wstring> candidates;
        std::vector<std::uint64_t> candidate_consumed;
    };
    struct PendingRequest {
        std::uint8_t type{};
        std::uint64_t request_id{};
        std::uint64_t generation{};
        std::uint64_t page{};
        std::wstring input;
        bool expanded{};
        bool correction_enabled{true};
    };
    enum class HitKind : std::uint8_t {
        candidate,
        previous_page,
        next_page,
        toggle_expanded,
    };
    struct HitTarget {
        HitKind kind{HitKind::candidate};
        std::size_t candidate_index{};

        bool operator==(const HitTarget&) const = default;
    };
    struct HitRegion {
        RECT bounds{};
        HitTarget target;
    };

    virtual ~TextService();
    static LRESULT CALLBACK window_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam);
    HRESULT initialize_windows();
    void destroy_windows() noexcept;
    HRESULT initialize_rendering();
    HRESULT ensure_device_resources();
    void discard_device_resources() noexcept;
    void discard_rendering() noexcept;
    void apply_candidate_window_effects() noexcept;
    void render_candidate_window();
    [[nodiscard]] SIZE desired_candidate_window_size() const;
    void worker_loop(std::stop_token stop_token);
    void queue_candidate_request();
    void schedule_candidate_request(bool reset_retry);
    void queue_commit_feedback(std::wstring candidate);
    void refresh_shortcut_config(bool force = false);
    void sync_preserved_language_key();
    void clear_preserved_language_key() noexcept;
    [[nodiscard]] bool shortcut_matches(std::string_view shortcut, WPARAM key) const;
    void handle_candidate_result(CandidateResult* result);
    void update_candidate_window();
    void change_candidate_page(int direction);
    void scroll_expanded_candidates(int rows);
    void invoke_hit_target(const HitTarget& target);
    void defer_candidate_selection(std::size_t index, ITfContext* context);
    void clear_deferred_candidate_selection() noexcept;
    [[nodiscard]] std::optional<HitTarget> hit_test(POINT point) const;
    void update_candidate_anchor(ITfContext* context);
    void clear_composition();
    [[nodiscard]] bool should_eat_key(WPARAM key) const noexcept;
    HRESULT commit_candidate(ITfContext* context, std::size_t index);
    HRESULT commit_raw_input(ITfContext* context);
    HRESULT commit_candidate_from_window(std::size_t index);
    HRESULT toggle_language_mode(ITfContext* context);

    LONG references_{1};
    ITfThreadMgr* thread_manager_{nullptr};
    TfClientId client_id_{TF_CLIENTID_NULL};
    DWORD thread_manager_event_sink_cookie_{TF_INVALID_COOKIE};
    DWORD thread_focus_sink_cookie_{TF_INVALID_COOKIE};
    HWND message_window_{nullptr};
    HWND candidate_window_{nullptr};
    ID2D1Factory* d2d_factory_{nullptr};
    IDWriteFactory* dwrite_factory_{nullptr};
    IDWriteTextFormat* input_text_format_{nullptr};
    IDWriteTextFormat* candidate_text_format_{nullptr};
    IDWriteTextFormat* label_text_format_{nullptr};
    ID2D1HwndRenderTarget* render_target_{nullptr};
    ID2D1SolidColorBrush* background_brush_{nullptr};
    ID2D1SolidColorBrush* border_brush_{nullptr};
    ID2D1SolidColorBrush* text_brush_{nullptr};
    ID2D1SolidColorBrush* secondary_text_brush_{nullptr};
    ID2D1SolidColorBrush* accent_brush_{nullptr};
    ID2D1SolidColorBrush* highlight_brush_{nullptr};
    ID2D1SolidColorBrush* strict_accent_brush_{nullptr};
    std::wstring input_buffer_;
    std::wstring segmented_input_;
    std::vector<std::wstring> candidates_;
    std::vector<std::uint64_t> candidate_consumed_;
    std::wstring candidate_failure_detail_;
    std::vector<HitRegion> hit_regions_;
    std::optional<HitTarget> hovered_target_;
    std::optional<HitTarget> pressed_target_;
    std::optional<std::wstring> deferred_candidate_text_;
    ITfContext* deferred_candidate_context_{nullptr};
    std::uint64_t context_generation_{0};
    std::uint64_t next_request_id_{1};
    std::uint64_t active_candidate_request_id_{0};
    std::uint64_t candidate_page_{0};
    std::uint64_t candidate_page_size_{5};
    bool has_more_candidates_{false};
    bool candidate_request_pending_{false};
    bool candidate_request_failed_{false};
    std::uint8_t candidate_retry_count_{0};
    bool candidates_expanded_{false};
    std::size_t expanded_scroll_row_{0};
    config::ConfigStore shortcut_config_store_;
    config::AppConfig shortcut_config_;
    ULONGLONG next_shortcut_config_refresh_{0};
    bool shortcut_config_initialized_{false};
    std::string preserved_language_shortcut_;
    TF_PRESERVEDKEY preserved_language_key_{};
    bool preserved_language_key_registered_{false};
    bool correction_enabled_{true};
    bool chinese_mode_{true};
    bool foreground_focus_{true};
    POINT candidate_anchor_{};
    bool candidate_anchor_valid_{false};
    std::mutex request_mutex_;
    std::condition_variable_any request_ready_;
    std::optional<PendingRequest> pending_request_;
    std::deque<PendingRequest> feedback_requests_;
    std::jthread worker_;
};

HRESULT create_text_service(REFIID iid, void** object);
void increment_server_lock() noexcept;
void decrement_server_lock() noexcept;
[[nodiscard]] long server_lock_count() noexcept;

}  // namespace owo::tsf
