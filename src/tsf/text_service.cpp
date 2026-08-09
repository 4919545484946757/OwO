#include "text_service.h"

#include "owo/config/config_paths.h"
#include "owo/ipc/named_pipe.h"
#include "owo/protocol/messages.h"

#include <d2d1.h>
#include <d2d1helper.h>
#include <dwmapi.h>
#include <dwrite.h>
#include <windowsx.h>

#include <algorithm>
#include <cmath>
#include <chrono>
#include <memory>
#include <new>
#include <string_view>
#include <utility>

namespace owo::tsf {
namespace {
LONG object_count = 0;
LONG lock_count = 0;
constexpr wchar_t kMessageClass[] = L"OwO.P1.MessageWindow";
constexpr wchar_t kCandidateClass[] = L"OwO.P1.CandidateWindow";
constexpr UINT kCandidateReady = WM_APP + 1;
constexpr float kCandidateWindowMinWidthDip = 240.0F;
constexpr float kCandidateWindowMaxWidthDip = 1400.0F;
constexpr float kHeaderHeightDip = 40.0F;
constexpr float kCandidateItemHeightDip = 32.0F;
constexpr float kCandidateTextLineHeightDip = 20.0F;
constexpr float kCandidateRowGapDip = 6.0F;
constexpr float kHorizontalPaddingDip = 14.0F;
constexpr float kCandidateGapDip = 6.0F;
constexpr float kCandidatePillBaseWidthDip = 45.0F;
constexpr float kButtonWidthDip = 30.0F;
constexpr float kExpandButtonWidthDip = 52.0F;
constexpr float kControlGapDip = 6.0F;
constexpr float kCandidateControlGapDip = 10.0F;
constexpr std::size_t kExpandedVisibleRows = 5;
constexpr std::size_t kMaximumPinyinInputLength = 256;
constexpr ULONGLONG kShortcutConfigRefreshIntervalMs = 500;
constexpr auto kCandidateRequestBaseTimeout = std::chrono::milliseconds(900);
constexpr auto kCandidateRequestMaximumTimeout = std::chrono::milliseconds(2500);
constexpr auto kFeedbackRequestTimeout = std::chrono::milliseconds(100);

std::wstring_view candidate_status_text(const bool pending, const bool failed,
                                        const std::wstring_view failure_detail) noexcept {
    if (pending) return L"正在查找…";
    if (!failed) return L"无候选";
    return failure_detail.empty() ? L"候选服务暂不可用" : failure_detail;
}

template <typename Interface>
void release_interface(Interface*& value) noexcept {
    if (value == nullptr) return;
    value->Release();
    value = nullptr;
}

float measure_text_width(IDWriteFactory* factory,
                         IDWriteTextFormat* format,
                         const std::wstring_view text) {
    if (factory == nullptr || format == nullptr || text.empty()) return 0.0F;
    IDWriteTextLayout* layout = nullptr;
    const HRESULT result = factory->CreateTextLayout(
        text.data(), static_cast<UINT32>(text.size()), format, 4096.0F,
        4096.0F, &layout);
    if (FAILED(result)) return 0.0F;
    DWRITE_TEXT_METRICS metrics{};
    const HRESULT metrics_result = layout->GetMetrics(&metrics);
    layout->Release();
    return SUCCEEDED(metrics_result) ? metrics.widthIncludingTrailingWhitespace : 0.0F;
}

std::size_t wrapped_line_count(const std::wstring_view text,
                               const std::size_t wrap_length) noexcept {
    if (text.empty()) return 1;
    return (text.size() + wrap_length - 1) / wrap_length;
}

float wrapped_candidate_height(const std::wstring_view text,
                               const std::size_t wrap_length) noexcept {
    return kCandidateItemHeightDip +
           static_cast<float>(wrapped_line_count(text, wrap_length) - 1) *
               kCandidateTextLineHeightDip;
}

std::wstring wrapped_candidate_text(const std::wstring_view text,
                                    const std::size_t wrap_length) {
    if (text.size() <= wrap_length) return std::wstring(text);
    std::wstring wrapped;
    wrapped.reserve(text.size() + text.size() / wrap_length);
    for (std::size_t offset = 0; offset < text.size(); offset += wrap_length) {
        if (!wrapped.empty()) wrapped.push_back(L'\n');
        wrapped.append(text.substr(offset, std::min(wrap_length, text.size() - offset)));
    }
    return wrapped;
}

int dips_to_pixels(const float dips, const UINT dpi) noexcept {
    return static_cast<int>(std::ceil(dips * static_cast<float>(dpi) / 96.0F));
}

RECT dip_rect_to_pixels(const D2D1_RECT_F bounds, const UINT dpi) noexcept {
    const float scale = static_cast<float>(dpi) / 96.0F;
    return RECT{static_cast<LONG>(std::floor(bounds.left * scale)),
                static_cast<LONG>(std::floor(bounds.top * scale)),
                static_cast<LONG>(std::ceil(bounds.right * scale)),
                static_cast<LONG>(std::ceil(bounds.bottom * scale))};
}

std::string utf8_from_wide(const std::wstring_view input) {
    if (input.empty()) return {};
    const int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, input.data(),
                                         static_cast<int>(input.size()), nullptr, 0,
                                         nullptr, nullptr);
    if (size <= 0) return {};
    std::string result(static_cast<std::size_t>(size), '\0');
    if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, input.data(),
                            static_cast<int>(input.size()), result.data(), size,
                            nullptr, nullptr) != size) {
        return {};
    }
    return result;
}

std::wstring wide_from_utf8(const std::string_view input) {
    if (input.empty()) return {};
    const int size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, input.data(),
                                         static_cast<int>(input.size()), nullptr, 0);
    if (size <= 0) return {};
    std::wstring result(static_cast<std::size_t>(size), L'\0');
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, input.data(),
                            static_cast<int>(input.size()), result.data(), size) != size) {
        return {};
    }
    return result;
}

bool key_down(const int key) noexcept {
    return (GetKeyState(key) & 0x8000) != 0;
}

bool is_control_key(const WPARAM key) noexcept {
    return key == VK_CONTROL || key == VK_LCONTROL || key == VK_RCONTROL;
}

bool is_alt_key(const WPARAM key) noexcept {
    return key == VK_MENU || key == VK_LMENU || key == VK_RMENU;
}

bool is_shift_key(const WPARAM key) noexcept {
    return key == VK_SHIFT || key == VK_LSHIFT || key == VK_RSHIFT;
}

std::string primary_shortcut_name(const WPARAM key) {
    if ((key >= 'A' && key <= 'Z') || (key >= '0' && key <= '9'))
        return std::string(1, static_cast<char>(key));
    if (key >= VK_F1 && key <= VK_F24)
        return "F" + std::to_string(key - VK_F1 + 1);
    switch (key) {
        case VK_SPACE: return "Space";
        case VK_RETURN: return "Enter";
        case VK_TAB: return "Tab";
        case VK_ESCAPE: return "Escape";
        case VK_BACK: return "Backspace";
        case VK_DELETE: return "Delete";
        case VK_INSERT: return "Insert";
        case VK_HOME: return "Home";
        case VK_END: return "End";
        case VK_PRIOR: return "PageUp";
        case VK_NEXT: return "PageDown";
        case VK_LEFT: return "Left";
        case VK_RIGHT: return "Right";
        case VK_UP: return "Up";
        case VK_DOWN: return "Down";
        case VK_OEM_4: return "[";
        case VK_OEM_6: return "]";
        case VK_OEM_MINUS: return "Minus";
        case VK_OEM_PLUS: return "Plus";
        case VK_OEM_COMMA: return "Comma";
        case VK_OEM_PERIOD: return "Period";
        case VK_OEM_2: return "Slash";
        case VK_OEM_1: return "Semicolon";
        case VK_OEM_7: return "Quote";
        case VK_OEM_3: return "Backtick";
        default: return {};
    }
}

std::string shortcut_for_key_event(const WPARAM key) {
    const bool control = is_control_key(key) || key_down(VK_CONTROL);
    const bool alt = is_alt_key(key) || key_down(VK_MENU);
    const bool shift = is_shift_key(key) || key_down(VK_SHIFT);
    std::string result;
    const auto append = [&result](const std::string_view token) {
        if (!result.empty()) result.push_back('+');
        result += token;
    };
    if (control) append("Ctrl");
    if (alt) append("Alt");
    if (shift) append("Shift");
    if (!is_control_key(key) && !is_alt_key(key) && !is_shift_key(key)) {
        const auto primary = primary_shortcut_name(key);
        if (primary.empty()) return {};
        append(primary);
    }
    return result;
}

bool command_modifier_down() noexcept {
    return key_down(VK_CONTROL) || key_down(VK_MENU) || key_down(VK_LWIN) ||
           key_down(VK_RWIN);
}

std::chrono::milliseconds candidate_request_timeout(const std::size_t input_length) {
    const auto length_allowance = std::chrono::milliseconds(input_length * 5);
    return std::min(kCandidateRequestBaseTimeout + length_allowance,
                    kCandidateRequestMaximumTimeout);
}

class CommitEditSession final : public ITfEditSession {
public:
    CommitEditSession(ITfContext* context, std::wstring text)
        : context_(context), text_(std::move(text)) {
        context_->AddRef();
    }

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
        if (object == nullptr) return E_POINTER;
        *object = nullptr;
        if (iid == IID_IUnknown || iid == IID_ITfEditSession) {
            *object = static_cast<ITfEditSession*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override {
        return static_cast<ULONG>(InterlockedIncrement(&references_));
    }
    ULONG STDMETHODCALLTYPE Release() override {
        const auto remaining = InterlockedDecrement(&references_);
        if (remaining == 0) delete this;
        return static_cast<ULONG>(remaining);
    }
    HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie cookie) override {
        TF_SELECTION selection{};
        ULONG fetched = 0;
        HRESULT result = context_->GetSelection(cookie, TF_DEFAULT_SELECTION, 1,
                                                &selection, &fetched);
        if (FAILED(result) || fetched != 1 || selection.range == nullptr) return result;
        result = selection.range->SetText(cookie, 0, text_.data(),
                                          static_cast<LONG>(text_.size()));
        if (SUCCEEDED(result)) {
            selection.range->Collapse(cookie, TF_ANCHOR_END);
            result = context_->SetSelection(cookie, 1, &selection);
        }
        selection.range->Release();
        return result;
    }

private:
    ~CommitEditSession() { context_->Release(); }
    LONG references_{1};
    ITfContext* context_;
    std::wstring text_;
};

class CaretEditSession final : public ITfEditSession {
public:
    CaretEditSession(ITfContext* context, POINT* anchor, bool* valid)
        : context_(context), anchor_(anchor), valid_(valid) {
        context_->AddRef();
    }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
        if (object == nullptr) return E_POINTER;
        *object = nullptr;
        if (iid == IID_IUnknown || iid == IID_ITfEditSession) {
            *object = static_cast<ITfEditSession*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override {
        return static_cast<ULONG>(InterlockedIncrement(&references_));
    }
    ULONG STDMETHODCALLTYPE Release() override {
        const auto remaining = InterlockedDecrement(&references_);
        if (remaining == 0) delete this;
        return static_cast<ULONG>(remaining);
    }
    HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie cookie) override {
        *valid_ = false;
        TF_SELECTION selection{};
        ULONG fetched = 0;
        HRESULT result = context_->GetSelection(cookie, TF_DEFAULT_SELECTION, 1,
                                                &selection, &fetched);
        if (FAILED(result) || fetched != 1 || selection.range == nullptr) return result;
        ITfContextView* view = nullptr;
        result = context_->GetActiveView(&view);
        if (SUCCEEDED(result)) {
            RECT bounds{};
            BOOL clipped = FALSE;
            result = view->GetTextExt(cookie, selection.range, &bounds, &clipped);
            if (SUCCEEDED(result)) {
                anchor_->x = bounds.left;
                anchor_->y = bounds.bottom;
                *valid_ = true;
            }
            view->Release();
        }
        selection.range->Release();
        return result;
    }

private:
    ~CaretEditSession() { context_->Release(); }
    LONG references_{1};
    ITfContext* context_;
    POINT* anchor_;
    bool* valid_;
};
}  // namespace

TextService::TextService() noexcept {
    InterlockedIncrement(&object_count);
}

TextService::~TextService() {
    Deactivate();
    InterlockedDecrement(&object_count);
}

HRESULT TextService::QueryInterface(REFIID iid, void** object) {
    if (object == nullptr) return E_POINTER;
    *object = nullptr;
    if (iid == IID_IUnknown || iid == IID_ITfTextInputProcessor ||
        iid == IID_ITfTextInputProcessorEx) {
        *object = static_cast<ITfTextInputProcessorEx*>(this);
    } else if (iid == IID_ITfKeyEventSink) {
        *object = static_cast<ITfKeyEventSink*>(this);
    } else if (iid == IID_ITfThreadMgrEventSink) {
        *object = static_cast<ITfThreadMgrEventSink*>(this);
    } else if (iid == IID_ITfThreadFocusSink) {
        *object = static_cast<ITfThreadFocusSink*>(this);
    } else {
        return E_NOINTERFACE;
    }
    AddRef();
    return S_OK;
}

ULONG TextService::AddRef() {
    return static_cast<ULONG>(InterlockedIncrement(&references_));
}

ULONG TextService::Release() {
    const auto remaining = InterlockedDecrement(&references_);
    if (remaining == 0) delete this;
    return static_cast<ULONG>(remaining);
}

HRESULT TextService::Activate(ITfThreadMgr* thread_manager, const TfClientId client_id) {
    return ActivateEx(thread_manager, client_id, 0);
}

HRESULT TextService::ActivateEx(ITfThreadMgr* thread_manager,
                                const TfClientId client_id,
                                DWORD) {
    if (thread_manager == nullptr) return E_INVALIDARG;
    if (thread_manager_ != nullptr) return E_UNEXPECTED;
    thread_manager_ = thread_manager;
    thread_manager_->AddRef();
    client_id_ = client_id;

    ITfKeystrokeMgr* keystroke_manager = nullptr;
    HRESULT result = thread_manager_->QueryInterface(IID_PPV_ARGS(&keystroke_manager));
    if (SUCCEEDED(result)) {
        result = keystroke_manager->AdviseKeyEventSink(client_id_, this, TRUE);
        keystroke_manager->Release();
    }
    if (FAILED(result)) {
        Deactivate();
        return result;
    }
    ITfSource* source = nullptr;
    result = thread_manager_->QueryInterface(IID_PPV_ARGS(&source));
    if (SUCCEEDED(result)) {
        result = source->AdviseSink(IID_ITfThreadMgrEventSink,
                                    static_cast<ITfThreadMgrEventSink*>(this),
                                    &thread_manager_event_sink_cookie_);
        if (SUCCEEDED(result)) {
            result = source->AdviseSink(IID_ITfThreadFocusSink,
                                        static_cast<ITfThreadFocusSink*>(this),
                                        &thread_focus_sink_cookie_);
        }
        source->Release();
    }
    if (FAILED(result)) {
        Deactivate();
        return result;
    }
    result = initialize_windows();
    if (FAILED(result)) {
        Deactivate();
        return result;
    }
    refresh_shortcut_config(true);
    worker_ = std::jthread([this](const std::stop_token token) { worker_loop(token); });
    return S_OK;
}

HRESULT TextService::Deactivate() {
    if (worker_.joinable()) {
        worker_.request_stop();
        request_ready_.notify_all();
        worker_.join();
    }
    {
        std::lock_guard lock(request_mutex_);
        pending_request_.reset();
        feedback_requests_.clear();
    }
    clear_composition();
    destroy_windows();
    if (thread_manager_ != nullptr) {
        ITfSource* source = nullptr;
        if (SUCCEEDED(thread_manager_->QueryInterface(IID_PPV_ARGS(&source)))) {
            if (thread_focus_sink_cookie_ != TF_INVALID_COOKIE)
                source->UnadviseSink(thread_focus_sink_cookie_);
            if (thread_manager_event_sink_cookie_ != TF_INVALID_COOKIE)
                source->UnadviseSink(thread_manager_event_sink_cookie_);
            source->Release();
        }
        thread_focus_sink_cookie_ = TF_INVALID_COOKIE;
        thread_manager_event_sink_cookie_ = TF_INVALID_COOKIE;
        ITfKeystrokeMgr* keystroke_manager = nullptr;
        if (SUCCEEDED(thread_manager_->QueryInterface(IID_PPV_ARGS(&keystroke_manager)))) {
            keystroke_manager->UnadviseKeyEventSink(client_id_);
            keystroke_manager->Release();
        }
        thread_manager_->Release();
        thread_manager_ = nullptr;
    }
    client_id_ = TF_CLIENTID_NULL;
    return S_OK;
}

HRESULT TextService::OnSetFocus(const BOOL foreground) {
    foreground_focus_ = foreground != FALSE;
    if (!foreground_focus_) {
        clear_composition();
    } else if (!input_buffer_.empty()) {
        update_candidate_window();
    }
    return S_OK;
}

HRESULT TextService::OnInitDocumentMgr(ITfDocumentMgr*) {
    return S_OK;
}

HRESULT TextService::OnUninitDocumentMgr(ITfDocumentMgr*) {
    return S_OK;
}

HRESULT TextService::OnSetFocus(ITfDocumentMgr* document_manager,
                                ITfDocumentMgr* previous_document_manager) {
    if (document_manager != previous_document_manager) clear_composition();
    foreground_focus_ = document_manager != nullptr;
    return S_OK;
}

HRESULT TextService::OnPushContext(ITfContext*) {
    clear_composition();
    return S_OK;
}

HRESULT TextService::OnPopContext(ITfContext*) {
    clear_composition();
    return S_OK;
}

HRESULT TextService::OnSetThreadFocus() {
    foreground_focus_ = true;
    if (!input_buffer_.empty()) update_candidate_window();
    return S_OK;
}

HRESULT TextService::OnKillThreadFocus() {
    foreground_focus_ = false;
    clear_composition();
    return S_OK;
}

bool TextService::should_eat_key(const WPARAM key) const noexcept {
    if (shortcut_config_.correction_shortcut_enabled &&
        shortcut_matches(shortcut_config_.correction_shortcut, key)) return true;
    if (shortcut_config_.language_shortcut_enabled &&
        shortcut_matches(shortcut_config_.language_shortcut, key)) return true;
    if (!input_buffer_.empty() && shortcut_config_.raw_input_shortcut_enabled &&
        shortcut_matches(shortcut_config_.raw_input_shortcut, key)) return true;
    if (!chinese_mode_) return false;
    if (key >= 'A' && key <= 'Z') return !command_modifier_down();
    if (input_buffer_.empty()) return false;
    if (key == VK_OEM_7) return GetKeyState(VK_SHIFT) >= 0 && !command_modifier_down();
    if (key == VK_BACK || key == VK_ESCAPE) return true;
    if ((key == VK_UP || key == VK_DOWN) && candidates_expanded_) return true;
    if (key == VK_NEXT || key == VK_OEM_6)
        return candidates_expanded_ ||
               (!candidate_request_pending_ && has_more_candidates_);
    if (key == VK_PRIOR || key == VK_OEM_4)
        return candidates_expanded_ ||
               (!candidate_request_pending_ && candidate_page_ > 0);
    if (key == VK_SPACE)
        return candidate_request_pending_ || !candidates_.empty();
    return key >= '1' && key <= '9' &&
           (candidate_request_pending_ ||
            static_cast<std::size_t>(key - '1') < candidates_.size());
}

HRESULT TextService::OnTestKeyDown(ITfContext*, WPARAM key, LPARAM, BOOL* eaten) {
    if (eaten == nullptr) return E_POINTER;
    refresh_shortcut_config();
    *eaten = should_eat_key(key) ? TRUE : FALSE;
    return S_OK;
}

HRESULT TextService::OnKeyDown(ITfContext* context, WPARAM key, LPARAM, BOOL* eaten) {
    if (eaten == nullptr) return E_POINTER;
    refresh_shortcut_config();
    *eaten = should_eat_key(key) ? TRUE : FALSE;
    if (!*eaten) return S_OK;
    if (context != nullptr) update_candidate_anchor(context);

    if (shortcut_config_.correction_shortcut_enabled &&
        shortcut_matches(shortcut_config_.correction_shortcut, key)) {
        correction_enabled_ = !correction_enabled_;
        if (!input_buffer_.empty()) {
            segmented_input_.clear();
            candidates_.clear();
            candidate_consumed_.clear();
            candidate_failure_detail_.clear();
            candidate_page_ = 0;
            has_more_candidates_ = false;
            candidates_expanded_ = false;
            ++context_generation_;
            queue_candidate_request();
        }
    } else if (shortcut_config_.language_shortcut_enabled &&
               shortcut_matches(shortcut_config_.language_shortcut, key)) {
        if (chinese_mode_ && !input_buffer_.empty()) {
            if (context != nullptr) {
                const HRESULT committed = commit_raw_input(context);
                if (FAILED(committed)) return committed;
            } else {
                clear_composition();
            }
        }
        chinese_mode_ = !chinese_mode_;
    } else if (!input_buffer_.empty() && shortcut_config_.raw_input_shortcut_enabled &&
               shortcut_matches(shortcut_config_.raw_input_shortcut, key)) {
        if (context != nullptr) return commit_raw_input(context);
        clear_composition();
    } else if (key >= 'A' && key <= 'Z') {
        if (input_buffer_.size() >= kMaximumPinyinInputLength) return S_OK;
        input_buffer_.push_back(static_cast<wchar_t>(L'a' + (key - 'A')));
        segmented_input_.clear();
        candidate_page_ = 0;
        has_more_candidates_ = false;
        candidates_expanded_ = false;
        ++context_generation_;
        queue_candidate_request();
    } else if (key == VK_OEM_7 && !input_buffer_.empty() &&
               input_buffer_.back() != L'\'' &&
               input_buffer_.size() < kMaximumPinyinInputLength) {
        input_buffer_.push_back(L'\'');
        segmented_input_.clear();
        candidate_page_ = 0;
        has_more_candidates_ = false;
        candidates_expanded_ = false;
        ++context_generation_;
        queue_candidate_request();
    } else if (key == VK_BACK) {
        if (!input_buffer_.empty()) input_buffer_.pop_back();
        segmented_input_.clear();
        candidate_page_ = 0;
        has_more_candidates_ = false;
        candidates_expanded_ = false;
        ++context_generation_;
        if (input_buffer_.empty()) clear_composition();
        else queue_candidate_request();
    } else if (key == VK_ESCAPE) {
        clear_composition();
    } else if (candidates_expanded_ &&
               (key == VK_DOWN || key == VK_NEXT || key == VK_OEM_6)) {
        scroll_expanded_candidates(1);
    } else if (candidates_expanded_ &&
               (key == VK_UP || key == VK_PRIOR || key == VK_OEM_4)) {
        scroll_expanded_candidates(-1);
    } else if (!candidates_expanded_ &&
               (key == VK_NEXT || key == VK_OEM_6) && has_more_candidates_) {
        change_candidate_page(1);
    } else if (!candidates_expanded_ &&
               (key == VK_PRIOR || key == VK_OEM_4) && candidate_page_ > 0) {
        change_candidate_page(-1);
    } else if (candidate_request_pending_) {
        const std::size_t index = key == VK_SPACE ? 0 : static_cast<std::size_t>(key - '1');
        defer_candidate_selection(index, context);
    } else if (context != nullptr && !candidates_.empty()) {
        const std::size_t index = key == VK_SPACE ? 0 : static_cast<std::size_t>(key - '1');
        if (index < candidates_.size()) return commit_candidate(context, index);
    }
    return S_OK;
}

HRESULT TextService::OnTestKeyUp(ITfContext*, const WPARAM key, LPARAM, BOOL* eaten) {
    if (eaten == nullptr) return E_POINTER;
    static_cast<void>(key);
    *eaten = FALSE;
    return S_OK;
}

HRESULT TextService::OnKeyUp(ITfContext*, const WPARAM key, LPARAM, BOOL* eaten) {
    if (eaten == nullptr) return E_POINTER;
    static_cast<void>(key);
    *eaten = FALSE;
    return S_OK;
}

HRESULT TextService::OnPreservedKey(ITfContext*, REFGUID, BOOL* eaten) {
    if (eaten == nullptr) return E_POINTER;
    *eaten = FALSE;
    return S_OK;
}

HRESULT TextService::initialize_windows() {
    HMODULE module = nullptr;
    if (!GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            reinterpret_cast<LPCWSTR>(&TextService::window_proc), &module)) {
        return HRESULT_FROM_WIN32(GetLastError());
    }
    WNDCLASSW message_class{};
    message_class.lpfnWndProc = window_proc;
    message_class.hInstance = module;
    message_class.lpszClassName = kMessageClass;
    if (RegisterClassW(&message_class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) {
        return HRESULT_FROM_WIN32(GetLastError());
    }
    WNDCLASSW candidate_class = message_class;
    candidate_class.style = CS_DROPSHADOW;
    candidate_class.hbrBackground = nullptr;
    candidate_class.lpszClassName = kCandidateClass;
    if (RegisterClassW(&candidate_class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) {
        return HRESULT_FROM_WIN32(GetLastError());
    }
    message_window_ = CreateWindowExW(0, kMessageClass, L"", 0, 0, 0, 0, 0,
                                      HWND_MESSAGE, nullptr, message_class.hInstance, this);
    if (message_window_ == nullptr) return HRESULT_FROM_WIN32(GetLastError());
    candidate_window_ = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                                        kCandidateClass, L"", WS_POPUP,
                                        0, 0, 780, 88, nullptr, nullptr,
                                        message_class.hInstance, this);
    if (candidate_window_ == nullptr) return HRESULT_FROM_WIN32(GetLastError());
    const HRESULT result = initialize_rendering();
    if (FAILED(result)) return result;
    apply_candidate_window_effects();
    return S_OK;
}

void TextService::destroy_windows() noexcept {
    discard_rendering();
    if (candidate_window_ != nullptr) DestroyWindow(candidate_window_);
    if (message_window_ != nullptr) DestroyWindow(message_window_);
    candidate_window_ = nullptr;
    message_window_ = nullptr;
}

HRESULT TextService::initialize_rendering() {
    HRESULT result = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, &d2d_factory_);
    if (FAILED(result)) return result;
    result = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, __uuidof(IDWriteFactory),
                                 reinterpret_cast<IUnknown**>(&dwrite_factory_));
    if (FAILED(result)) {
        discard_rendering();
        return result;
    }

    result = dwrite_factory_->CreateTextFormat(
        L"Microsoft YaHei UI", nullptr, DWRITE_FONT_WEIGHT_SEMI_BOLD,
        DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 15.0F, L"zh-CN",
        &input_text_format_);
    if (SUCCEEDED(result)) {
        result = dwrite_factory_->CreateTextFormat(
            L"Microsoft YaHei UI", nullptr, DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 15.0F, L"zh-CN",
            &candidate_text_format_);
    }
    if (SUCCEEDED(result)) {
        result = dwrite_factory_->CreateTextFormat(
            L"Microsoft YaHei UI", nullptr, DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 11.0F, L"zh-CN",
            &label_text_format_);
    }
    if (FAILED(result)) {
        discard_rendering();
        return result;
    }

    input_text_format_->SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
    input_text_format_->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    candidate_text_format_->SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
    candidate_text_format_->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    label_text_format_->SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
    label_text_format_->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    label_text_format_->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
    return S_OK;
}

HRESULT TextService::ensure_device_resources() {
    if (render_target_ != nullptr) return S_OK;
    if (candidate_window_ == nullptr || d2d_factory_ == nullptr) return E_UNEXPECTED;

    RECT bounds{};
    GetClientRect(candidate_window_, &bounds);
    const UINT dpi = std::max(GetDpiForWindow(candidate_window_), 96U);
    const D2D1_SIZE_U pixel_size = D2D1::SizeU(
        static_cast<UINT32>(std::max(bounds.right - bounds.left, 1L)),
        static_cast<UINT32>(std::max(bounds.bottom - bounds.top, 1L)));
    HRESULT result = d2d_factory_->CreateHwndRenderTarget(
        D2D1::RenderTargetProperties(),
        D2D1::HwndRenderTargetProperties(candidate_window_, pixel_size), &render_target_);
    if (FAILED(result)) return result;
    render_target_->SetDpi(static_cast<float>(dpi), static_cast<float>(dpi));

    const auto create_brush = [this](const D2D1_COLOR_F color,
                                     ID2D1SolidColorBrush** brush) {
        return render_target_->CreateSolidColorBrush(color, brush);
    };
    result = create_brush(D2D1::ColorF(0x202124, 1.0F), &background_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0xFFFFFF, 0.14F), &border_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0xFFFFFF, 0.96F), &text_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0xFFFFFF, 0.62F), &secondary_text_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0x8AB4F8, 1.0F), &accent_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0x8AB4F8, 0.18F), &highlight_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0x2D2418, 1.0F),
                              &strict_background_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0xFFB74D, 1.0F), &strict_accent_brush_);
    if (SUCCEEDED(result))
        result = create_brush(D2D1::ColorF(0xFFB74D, 0.20F), &strict_highlight_brush_);
    if (FAILED(result)) discard_device_resources();
    return result;
}

void TextService::discard_device_resources() noexcept {
    release_interface(strict_highlight_brush_);
    release_interface(strict_accent_brush_);
    release_interface(strict_background_brush_);
    release_interface(highlight_brush_);
    release_interface(accent_brush_);
    release_interface(secondary_text_brush_);
    release_interface(text_brush_);
    release_interface(border_brush_);
    release_interface(background_brush_);
    release_interface(render_target_);
}

void TextService::discard_rendering() noexcept {
    discard_device_resources();
    release_interface(label_text_format_);
    release_interface(candidate_text_format_);
    release_interface(input_text_format_);
    release_interface(dwrite_factory_);
    release_interface(d2d_factory_);
}

void TextService::apply_candidate_window_effects() noexcept {
    if (candidate_window_ == nullptr) return;
    const BOOL dark_mode = TRUE;
    DwmSetWindowAttribute(candidate_window_, DWMWA_USE_IMMERSIVE_DARK_MODE,
                          &dark_mode, sizeof(dark_mode));
    const DWM_WINDOW_CORNER_PREFERENCE corners = DWMWCP_ROUND;
    DwmSetWindowAttribute(candidate_window_, DWMWA_WINDOW_CORNER_PREFERENCE,
                          &corners, sizeof(corners));
    const DWM_SYSTEMBACKDROP_TYPE backdrop = DWMSBT_TRANSIENTWINDOW;
    DwmSetWindowAttribute(candidate_window_, DWMWA_SYSTEMBACKDROP_TYPE,
                          &backdrop, sizeof(backdrop));
    const COLORREF border_color = DWMWA_COLOR_NONE;
    DwmSetWindowAttribute(candidate_window_, DWMWA_BORDER_COLOR,
                          &border_color, sizeof(border_color));
    const MARGINS margins{1, 1, 1, 1};
    DwmExtendFrameIntoClientArea(candidate_window_, &margins);
}

SIZE TextService::desired_candidate_window_size() const {
    const UINT dpi = candidate_window_ == nullptr
                         ? 96U
                         : std::max(GetDpiForWindow(candidate_window_), 96U);
    const auto page_size = std::max<std::size_t>(
        1, static_cast<std::size_t>(candidate_page_size_));
    const auto wrap_length = std::max<std::size_t>(
        1, static_cast<std::size_t>(shortcut_config_.candidate_wrap_length));
    const auto row_height = [this, wrap_length](const std::size_t begin,
                                                const std::size_t end) {
        float height = kCandidateItemHeightDip;
        for (std::size_t index = begin; index < end; ++index)
            height = std::max(height,
                              wrapped_candidate_height(candidates_[index], wrap_length));
        return height;
    };
    float content_height = kCandidateItemHeightDip;
    if (!candidates_.empty() && candidates_expanded_) {
        const auto first = std::min(candidates_.size(), expanded_scroll_row_ * page_size);
        const auto visible_end = std::min(
            candidates_.size(), first + kExpandedVisibleRows * page_size);
        content_height = 0.0F;
        std::size_t rows = 0;
        for (std::size_t begin = first; begin < visible_end; begin += page_size) {
            if (rows++ != 0) content_height += kCandidateRowGapDip;
            content_height += row_height(begin, std::min(visible_end, begin + page_size));
        }
    } else if (!candidates_.empty()) {
        content_height = row_height(0, candidates_.size());
    }
    const float height = kHeaderHeightDip + 16.0F + content_height;
    const float controls_width = candidates_expanded_
                                     ? kExpandButtonWidthDip
                                     : kButtonWidthDip * 2.0F + kExpandButtonWidthDip +
                                           kControlGapDip * 2.0F;
    float candidates_width = 0.0F;
    if (candidates_.empty()) {
        const auto status = candidate_status_text(candidate_request_pending_,
                                                  candidate_request_failed_,
                                                  candidate_failure_detail_);
        candidates_width = kCandidatePillBaseWidthDip +
                           measure_text_width(dwrite_factory_, candidate_text_format_, status);
    } else if (candidates_expanded_) {
        for (std::size_t begin = 0; begin < candidates_.size(); begin += page_size) {
            const auto end = std::min(candidates_.size(), begin + page_size);
            float row_width = 0.0F;
            for (std::size_t index = begin; index < end; ++index) {
                if (row_width != 0.0F) row_width += kCandidateGapDip;
                const auto display = wrapped_candidate_text(candidates_[index], wrap_length);
                row_width += kCandidatePillBaseWidthDip +
                             measure_text_width(dwrite_factory_, candidate_text_format_,
                                                display);
            }
            candidates_width = std::max(candidates_width, row_width);
        }
    } else {
        for (const auto& candidate : candidates_) {
            if (candidates_width != 0.0F) candidates_width += kCandidateGapDip;
            const auto display = wrapped_candidate_text(candidate, wrap_length);
            candidates_width += kCandidatePillBaseWidthDip +
                                measure_text_width(dwrite_factory_, candidate_text_format_,
                                                   display);
        }
    }
    const std::wstring& reading = segmented_input_.empty() ? input_buffer_ : segmented_input_;
    const float preview_width = kHorizontalPaddingDip * 2.0F +
                                measure_text_width(dwrite_factory_, input_text_format_, reading);
    const float candidate_row_width = kHorizontalPaddingDip * 2.0F + candidates_width +
                                      kCandidateControlGapDip + controls_width;
    const float width = std::clamp(std::max(preview_width, candidate_row_width),
                                   kCandidateWindowMinWidthDip,
                                   kCandidateWindowMaxWidthDip);
    return SIZE{dips_to_pixels(width, dpi),
                dips_to_pixels(height, dpi)};
}

void TextService::render_candidate_window() {
    hit_regions_.clear();
    if (FAILED(ensure_device_resources())) return;
    render_target_->BeginDraw();
    render_target_->SetTransform(D2D1::Matrix3x2F::Identity());
    render_target_->SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    auto* mode_background_brush = correction_enabled_ ? background_brush_
                                                       : strict_background_brush_;
    auto* mode_accent_brush = correction_enabled_ ? accent_brush_ : strict_accent_brush_;
    auto* mode_highlight_brush = correction_enabled_ ? highlight_brush_
                                                      : strict_highlight_brush_;
    auto* mode_border_brush = correction_enabled_ ? border_brush_ : strict_accent_brush_;
    render_target_->Clear(correction_enabled_ ? D2D1::ColorF(0x202124, 1.0F)
                                              : D2D1::ColorF(0x2D2418, 1.0F));

    const D2D1_SIZE_F size = render_target_->GetSize();
    const UINT dpi = std::max(GetDpiForWindow(candidate_window_), 96U);
    const D2D1_ROUNDED_RECT card{
        D2D1::RectF(0.5F, 0.5F, size.width - 0.5F, size.height - 0.5F), 10.0F, 10.0F};
    render_target_->FillRoundedRectangle(card, mode_background_brush);
    render_target_->DrawRoundedRectangle(card, mode_border_brush, 1.0F);

    const std::wstring& reading = segmented_input_.empty() ? input_buffer_ : segmented_input_;
    const std::wstring& preview = reading;
    render_target_->DrawTextW(
        preview.data(), static_cast<UINT32>(preview.size()), input_text_format_,
        D2D1::RectF(kHorizontalPaddingDip, 0.0F, size.width - kHorizontalPaddingDip,
                    kHeaderHeightDip),
        mode_accent_brush, D2D1_DRAW_TEXT_OPTIONS_CLIP);
    render_target_->DrawLine(
        D2D1::Point2F(10.0F, kHeaderHeightDip),
        D2D1::Point2F(size.width - 10.0F, kHeaderHeightDip), mode_border_brush, 1.0F);

    const auto target_matches = [](const std::optional<HitTarget>& value,
                                   const HitTarget target) {
        return value.has_value() && *value == target;
    };
    const auto add_hit_region = [this, dpi](const D2D1_RECT_F bounds,
                                            const HitTarget target) {
        hit_regions_.push_back({dip_rect_to_pixels(bounds, dpi), target});
    };
    const auto wrap_length = std::max<std::size_t>(
        1, static_cast<std::size_t>(shortcut_config_.candidate_wrap_length));
    const auto draw_candidate = [this, &target_matches, &add_hit_region, wrap_length,
                                 mode_accent_brush, mode_border_brush,
                                 mode_highlight_brush](
                                    const D2D1_RECT_F bounds, const std::size_t index) {
        const HitTarget target{HitKind::candidate, index};
        const D2D1_ROUNDED_RECT pill{bounds, 7.0F, 7.0F};
        const bool hovered = target_matches(hovered_target_, target);
        const bool pressed = target_matches(pressed_target_, target);
        if (index == 0 || hovered || pressed)
            render_target_->FillRoundedRectangle(pill, mode_highlight_brush);
        if (hovered || pressed)
            render_target_->DrawRoundedRectangle(pill, mode_accent_brush, 1.0F);

        const float badge_center = (bounds.top + bounds.bottom) * 0.5F;
        const D2D1_ROUNDED_RECT badge{
            D2D1::RectF(bounds.left + 6.0F, badge_center - 11.0F,
                        bounds.left + 26.0F, badge_center + 11.0F),
            5.0F, 5.0F};
        render_target_->FillRoundedRectangle(badge, mode_border_brush);
        const std::wstring label = std::to_wstring(index + 1);
        render_target_->DrawTextW(label.data(), static_cast<UINT32>(label.size()),
                                  label_text_format_, badge.rect, mode_accent_brush,
                                  D2D1_DRAW_TEXT_OPTIONS_CLIP);
        const auto display = wrapped_candidate_text(candidates_[index], wrap_length);
        render_target_->DrawTextW(
            display.data(), static_cast<UINT32>(display.size()),
            candidate_text_format_,
            D2D1::RectF(bounds.left + 33.0F, bounds.top, bounds.right - 6.0F,
                        bounds.bottom),
            text_brush_, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        add_hit_region(bounds, target);
    };

    constexpr float content_top = kHeaderHeightDip + 8.0F;
    const float controls_width = candidates_expanded_
                                     ? kExpandButtonWidthDip
                                     : kButtonWidthDip * 2.0F + kExpandButtonWidthDip +
                                           kControlGapDip * 2.0F;
    const float controls_left = size.width - kHorizontalPaddingDip - controls_width;
    const float candidate_right = controls_left - kCandidateControlGapDip;

    if (candidates_.empty()) {
        const auto status = candidate_status_text(candidate_request_pending_,
                                                  candidate_request_failed_,
                                                  candidate_failure_detail_);
        render_target_->DrawTextW(
            status.data(), static_cast<UINT32>(status.size()), candidate_text_format_,
            D2D1::RectF(kHorizontalPaddingDip, content_top, candidate_right,
                        content_top + kCandidateItemHeightDip),
            secondary_text_brush_, D2D1_DRAW_TEXT_OPTIONS_CLIP);
    } else if (candidates_expanded_) {
        const auto page_size = std::max<std::size_t>(
            1, static_cast<std::size_t>(candidate_page_size_));
        const float available_width = std::max(1.0F, candidate_right - kHorizontalPaddingDip);
        const auto first_candidate = std::min(
            candidates_.size(), expanded_scroll_row_ * page_size);
        const auto visible_end = std::min(
            candidates_.size(), first_candidate + kExpandedVisibleRows * page_size);
        float top = content_top;
        for (std::size_t begin = first_candidate; begin < visible_end;
             begin += page_size) {
            const auto end = std::min(candidates_.size(), begin + page_size);
            const auto item_count = end - begin;
            float row_height = kCandidateItemHeightDip;
            const float gaps_width = item_count > 1
                                         ? kCandidateGapDip * static_cast<float>(item_count - 1)
                                         : 0.0F;
            float requested_total = 0.0F;
            for (std::size_t index = begin; index < end; ++index) {
                const auto display = wrapped_candidate_text(candidates_[index], wrap_length);
                requested_total += kCandidatePillBaseWidthDip +
                                   measure_text_width(dwrite_factory_, candidate_text_format_,
                                                      display);
                row_height = std::max(
                    row_height, wrapped_candidate_height(candidates_[index], wrap_length));
            }
            const float usable_width = std::max(1.0F, available_width - gaps_width);
            const float width_scale = requested_total > usable_width
                                          ? usable_width / requested_total
                                          : 1.0F;
            float x = kHorizontalPaddingDip;
            for (std::size_t index = begin; index < end; ++index) {
                const auto display = wrapped_candidate_text(candidates_[index], wrap_length);
                const float requested_width =
                    kCandidatePillBaseWidthDip +
                    measure_text_width(dwrite_factory_, candidate_text_format_,
                                       display);
                const float item_width = requested_width * width_scale;
                draw_candidate(D2D1::RectF(x, top, x + item_width, top + row_height),
                               index);
                x += item_width + kCandidateGapDip;
            }
            top += row_height + kCandidateRowGapDip;
        }
    } else {
        float x = kHorizontalPaddingDip;
        float row_height = kCandidateItemHeightDip;
        for (const auto& candidate : candidates_)
            row_height = std::max(row_height,
                                  wrapped_candidate_height(candidate, wrap_length));
        for (std::size_t index = 0; index < candidates_.size(); ++index) {
            const auto display = wrapped_candidate_text(candidates_[index], wrap_length);
            const float requested_width =
                kCandidatePillBaseWidthDip +
                measure_text_width(dwrite_factory_, candidate_text_format_,
                                   display);
            if (x + requested_width > candidate_right) break;
            draw_candidate(D2D1::RectF(x, content_top, x + requested_width,
                                       content_top + row_height),
                           index);
            x += requested_width + kCandidateGapDip;
        }
    }

    const auto draw_button = [this, &target_matches, &add_hit_region,
                              mode_accent_brush, mode_border_brush,
                              mode_highlight_brush](
                                 const D2D1_RECT_F bounds, const HitTarget target,
                                 const std::wstring_view label, const bool enabled) {
        const D2D1_ROUNDED_RECT button{bounds, 7.0F, 7.0F};
        const bool hovered = enabled && target_matches(hovered_target_, target);
        const bool pressed = enabled && target_matches(pressed_target_, target);
        if (hovered || pressed)
            render_target_->FillRoundedRectangle(button, mode_highlight_brush);
        render_target_->DrawRoundedRectangle(button, mode_border_brush, 1.0F);
        render_target_->DrawTextW(
            label.data(), static_cast<UINT32>(label.size()), label_text_format_, bounds,
            enabled ? (hovered || pressed ? mode_accent_brush : text_brush_)
                    : secondary_text_brush_,
            D2D1_DRAW_TEXT_OPTIONS_CLIP);
        if (enabled) add_hit_region(bounds, target);
    };

    float control_x = controls_left;
    if (!candidates_expanded_) {
        const D2D1_RECT_F previous_bounds =
            D2D1::RectF(control_x, content_top, control_x + kButtonWidthDip,
                        content_top + kCandidateItemHeightDip);
        draw_button(previous_bounds, {HitKind::previous_page, 0}, L"◀",
                    !candidate_request_pending_ && candidate_page_ > 0);
        control_x += kButtonWidthDip + kControlGapDip;
        const D2D1_RECT_F next_bounds =
            D2D1::RectF(control_x, content_top, control_x + kButtonWidthDip,
                        content_top + kCandidateItemHeightDip);
        draw_button(next_bounds, {HitKind::next_page, 0}, L"▶",
                    !candidate_request_pending_ && has_more_candidates_);
        control_x += kButtonWidthDip + kControlGapDip;
    }
    const D2D1_RECT_F expand_bounds =
        D2D1::RectF(control_x, content_top, control_x + kExpandButtonWidthDip,
                    content_top + kCandidateItemHeightDip);
    draw_button(expand_bounds, {HitKind::toggle_expanded, 0},
                candidates_expanded_ ? L"收起" : L"展开",
                !candidate_request_pending_ &&
                    (!candidates_.empty() || candidates_expanded_));

    const HRESULT result = render_target_->EndDraw();
    if (FAILED(result)) {
        discard_device_resources();
        if (candidate_window_ != nullptr) InvalidateRect(candidate_window_, nullptr, FALSE);
    }
}

LRESULT CALLBACK TextService::window_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    auto* service = reinterpret_cast<TextService*>(GetWindowLongPtrW(window, GWLP_USERDATA));
    if (message == WM_NCCREATE) {
        const auto* creation = reinterpret_cast<CREATESTRUCTW*>(lparam);
        service = static_cast<TextService*>(creation->lpCreateParams);
        SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(service));
    }
    if (message == kCandidateReady && service != nullptr) {
        service->handle_candidate_result(reinterpret_cast<CandidateResult*>(lparam));
        return 0;
    }
    if (message == WM_PAINT && service != nullptr) {
        PAINTSTRUCT paint{};
        BeginPaint(window, &paint);
        if (window == service->candidate_window_) service->render_candidate_window();
        EndPaint(window, &paint);
        return 0;
    }
    if (message == WM_ERASEBKGND && service != nullptr &&
        window == service->candidate_window_) {
        return 1;
    }
    if (message == WM_MOUSEACTIVATE && service != nullptr &&
        window == service->candidate_window_) {
        return MA_NOACTIVATE;
    }
    if (message == WM_MOUSEWHEEL && service != nullptr &&
        window == service->candidate_window_) {
        const int delta = GET_WHEEL_DELTA_WPARAM(wparam);
        const int notches = std::max(1, (delta < 0 ? -delta : delta) / WHEEL_DELTA);
        service->scroll_expanded_candidates(delta > 0 ? -notches : notches);
        return 0;
    }
    if (message == WM_MOUSEMOVE && service != nullptr &&
        window == service->candidate_window_) {
        const POINT point{GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
        const auto target = service->hit_test(point);
        if (target != service->hovered_target_) {
            service->hovered_target_ = target;
            InvalidateRect(window, nullptr, FALSE);
        }
        TRACKMOUSEEVENT tracking{sizeof(tracking), TME_LEAVE, window, 0};
        TrackMouseEvent(&tracking);
        return 0;
    }
    if (message == WM_MOUSELEAVE && service != nullptr &&
        window == service->candidate_window_) {
        service->hovered_target_.reset();
        InvalidateRect(window, nullptr, FALSE);
        return 0;
    }
    if (message == WM_LBUTTONDOWN && service != nullptr &&
        window == service->candidate_window_) {
        const POINT point{GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
        service->pressed_target_ = service->hit_test(point);
        if (service->pressed_target_) SetCapture(window);
        InvalidateRect(window, nullptr, FALSE);
        return 0;
    }
    if (message == WM_LBUTTONUP && service != nullptr &&
        window == service->candidate_window_) {
        const POINT point{GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
        const auto released_target = service->hit_test(point);
        const auto pressed_target = service->pressed_target_;
        service->pressed_target_.reset();
        if (GetCapture() == window) ReleaseCapture();
        InvalidateRect(window, nullptr, FALSE);
        if (pressed_target && released_target && *pressed_target == *released_target)
            service->invoke_hit_target(*released_target);
        return 0;
    }
    if (message == WM_CAPTURECHANGED && service != nullptr &&
        window == service->candidate_window_) {
        service->pressed_target_.reset();
        InvalidateRect(window, nullptr, FALSE);
        return 0;
    }
    if (message == WM_SETCURSOR && service != nullptr &&
        window == service->candidate_window_ && LOWORD(lparam) == HTCLIENT) {
        POINT point{};
        GetCursorPos(&point);
        ScreenToClient(window, &point);
        if (service->hit_test(point)) {
            SetCursor(LoadCursorW(nullptr, IDC_HAND));
            return TRUE;
        }
    }
    if (message == WM_SIZE && service != nullptr &&
        window == service->candidate_window_ && service->render_target_ != nullptr) {
        const HRESULT result = service->render_target_->Resize(
            D2D1::SizeU(static_cast<UINT32>(LOWORD(lparam)),
                        static_cast<UINT32>(HIWORD(lparam))));
        if (FAILED(result)) service->discard_device_resources();
        return 0;
    }
    if (message == WM_DPICHANGED && service != nullptr &&
        window == service->candidate_window_) {
        const auto* suggested = reinterpret_cast<const RECT*>(lparam);
        SetWindowPos(window, nullptr, suggested->left, suggested->top,
                     suggested->right - suggested->left, suggested->bottom - suggested->top,
                     SWP_NOACTIVATE | SWP_NOZORDER);
        service->discard_device_resources();
        InvalidateRect(window, nullptr, FALSE);
        return 0;
    }
    if ((message == WM_THEMECHANGED || message == WM_DWMCOLORIZATIONCOLORCHANGED) &&
        service != nullptr && window == service->candidate_window_) {
        service->apply_candidate_window_effects();
        InvalidateRect(window, nullptr, FALSE);
        return 0;
    }
    if (message == WM_DISPLAYCHANGE && service != nullptr &&
        window == service->candidate_window_) {
        service->discard_device_resources();
        InvalidateRect(window, nullptr, FALSE);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

void TextService::queue_candidate_request() {
    schedule_candidate_request(true);
}

void TextService::refresh_shortcut_config(const bool force) {
    const auto now = GetTickCount64();
    if (!force && shortcut_config_initialized_ && now < next_shortcut_config_refresh_) return;
    next_shortcut_config_refresh_ = now + kShortcutConfigRefreshIntervalMs;
    if (!shortcut_config_initialized_) {
        const auto path = config::default_config_path();
        if (path.empty()) return;
        const auto loaded = shortcut_config_store_.load(path);
        if (!loaded.success) return;
        shortcut_config_initialized_ = true;
        shortcut_config_ = shortcut_config_store_.snapshot();
        return;
    }
    const auto reloaded = shortcut_config_store_.reload();
    if (reloaded.success) shortcut_config_ = shortcut_config_store_.snapshot();
}

bool TextService::shortcut_matches(const std::string_view shortcut,
                                   const WPARAM key) const {
    return shortcut == shortcut_for_key_event(key);
}

void TextService::schedule_candidate_request(const bool reset_retry) {
    candidate_request_pending_ = true;
    candidate_request_failed_ = false;
    candidate_failure_detail_.clear();
    if (reset_retry) candidate_retry_count_ = 0;
    clear_deferred_candidate_selection();
    hovered_target_.reset();
    pressed_target_.reset();
    hit_regions_.clear();
    if (!candidates_expanded_) expanded_scroll_row_ = 0;
    {
        std::lock_guard lock(request_mutex_);
        active_candidate_request_id_ = next_request_id_++;
        pending_request_ = PendingRequest{
            static_cast<std::uint8_t>(protocol::MessageType::candidate_request),
            active_candidate_request_id_, context_generation_, candidate_page_, input_buffer_,
            candidates_expanded_, correction_enabled_};
    }
    request_ready_.notify_one();
    update_candidate_window();
}

void TextService::queue_commit_feedback(std::wstring candidate) {
    std::lock_guard lock(request_mutex_);
    feedback_requests_.push_back(PendingRequest{
        static_cast<std::uint8_t>(protocol::MessageType::candidate_committed),
        next_request_id_++, context_generation_, 0, std::move(candidate)});
    request_ready_.notify_one();
}

void TextService::worker_loop(const std::stop_token stop_token) {
    while (!stop_token.stop_requested()) {
        PendingRequest request{};
        {
            std::unique_lock lock(request_mutex_);
            request_ready_.wait(lock, stop_token, [this] {
                return pending_request_.has_value() || !feedback_requests_.empty();
            });
            if (stop_token.stop_requested()) break;
            if (pending_request_.has_value()) {
                request = std::move(*pending_request_);
                pending_request_.reset();
            } else {
                request = std::move(feedback_requests_.front());
                feedback_requests_.pop_front();
            }
        }
        const auto request_type = static_cast<protocol::MessageType>(request.type);
        const protocol::Message message{request_type,
                                        request.request_id, request.generation,
                                        utf8_from_wide(request.input)};
        auto paged_message = message;
        paged_message.page = request.page;
        paged_message.expanded = request.expanded;
        paged_message.correction_enabled = request.correction_enabled;
        const auto post_candidate_failure = [this, &request, request_type](
                                                std::wstring detail) {
            if (request_type != protocol::MessageType::candidate_request) return;
            auto result = std::make_unique<CandidateResult>();
            result->request_id = request.request_id;
            result->generation = request.generation;
            result->page = request.page;
            result->expanded = request.expanded;
            result->request_failed = true;
            result->failure_detail = std::move(detail);
            if (PostMessageW(message_window_, kCandidateReady, 0,
                             reinterpret_cast<LPARAM>(result.get())))
                result.release();
        };
        const auto timeout = request_type == protocol::MessageType::candidate_request
                                 ? candidate_request_timeout(request.input.size())
                                 : kFeedbackRequestTimeout;
        const auto exchanged = ipc::exchange(ipc::kCorePipeName,
                                             protocol::encode_message(paged_message),
                                             timeout);
        if (!exchanged.status || stop_token.stop_requested()) {
            if (!stop_token.stop_requested()) {
                auto detail = exchanged.status.error == protocol::ErrorCode::timeout
                                  ? std::wstring(L"候选生成稍慢，请继续输入或重试")
                                  : std::wstring(L"候选服务暂不可用");
                post_candidate_failure(std::move(detail));
            }
            continue;
        }
        const auto decoded = protocol::decode_message(exchanged.response);
        if (!decoded.validation) {
            auto detail = wide_from_utf8(decoded.validation.message);
            if (detail.empty()) detail = L"候选响应协议无效";
            post_candidate_failure(std::move(detail));
            continue;
        }
        if (decoded.message.request_id != request.request_id) {
            post_candidate_failure(L"候选响应 request ID 不匹配");
            continue;
        }
        if (request_type == protocol::MessageType::candidate_committed) {
            if (decoded.message.type != protocol::MessageType::acknowledgement) continue;
            continue;
        }
        if (decoded.message.type != protocol::MessageType::candidate_response) {
            post_candidate_failure(L"候选响应类型错误");
            continue;
        }
        if (decoded.message.correction_enabled != request.correction_enabled) {
            post_candidate_failure(L"候选响应纠错模式不匹配");
            continue;
        }
        auto result = std::make_unique<CandidateResult>();
        result->request_id = decoded.message.request_id;
        result->generation = decoded.message.context_generation;
        result->page = decoded.message.page;
        result->has_more = decoded.message.has_more;
        result->expanded = decoded.message.expanded;
        result->page_size = decoded.message.page_size;
        for (const auto& syllable : decoded.message.syllables) {
            auto converted = wide_from_utf8(syllable);
            if (converted.empty()) continue;
            if (!result->segmented_input.empty()) result->segmented_input.push_back(L'\'');
            result->segmented_input += converted;
        }
        result->candidates.reserve(decoded.message.candidates.size());
        result->candidate_consumed.reserve(decoded.message.candidate_consumed.size());
        for (std::size_t index = 0; index < decoded.message.candidates.size(); ++index) {
            auto converted = wide_from_utf8(decoded.message.candidates[index]);
            if (converted.empty()) continue;
            result->candidates.push_back(std::move(converted));
            result->candidate_consumed.push_back(
                decoded.message.candidate_consumed[index]);
        }
        if (PostMessageW(message_window_, kCandidateReady, 0,
                         reinterpret_cast<LPARAM>(result.get()))) {
            result.release();
        }
        if (!decoded.message.model_pending) continue;

        for (int attempt = 0; attempt < 6 && !stop_token.stop_requested(); ++attempt) {
            {
                std::unique_lock lock(request_mutex_);
                const bool interrupted = request_ready_.wait_for(
                    lock, stop_token, std::chrono::milliseconds(10), [this] {
                        return pending_request_.has_value() || !feedback_requests_.empty();
                    });
                if (interrupted || stop_token.stop_requested()) break;
            }
            const protocol::Message update_request{
                protocol::MessageType::candidate_update_request,
                request.request_id, request.generation, {}};
            const auto update_exchange = ipc::exchange(
                ipc::kCorePipeName, protocol::encode_message(update_request),
                std::chrono::milliseconds(25));
            if (!update_exchange.status) break;
            const auto update = protocol::decode_message(update_exchange.response);
            if (!update.validation ||
                update.message.type != protocol::MessageType::candidate_update_response ||
                update.message.request_id != request.request_id ||
                update.message.context_generation != request.generation) break;
            if (update.message.model_pending) continue;
            if (update.message.candidates.empty()) break;
            auto intelligent = std::make_unique<CandidateResult>();
            intelligent->request_id = request.request_id;
            intelligent->generation = request.generation;
            intelligent->page = request.page;
            intelligent->expanded = request.expanded;
            intelligent->preserve_paging = true;
            for (std::size_t index = 0; index < update.message.candidates.size(); ++index) {
                auto converted = wide_from_utf8(update.message.candidates[index]);
                if (converted.empty()) continue;
                intelligent->candidates.push_back(std::move(converted));
                intelligent->candidate_consumed.push_back(
                    update.message.candidate_consumed[index]);
            }
            if (!intelligent->candidates.empty() &&
                PostMessageW(message_window_, kCandidateReady, 0,
                             reinterpret_cast<LPARAM>(intelligent.get())))
                intelligent.release();
            break;
        }
    }
}

void TextService::handle_candidate_result(CandidateResult* raw_result) {
    std::unique_ptr<CandidateResult> result(raw_result);
    if (result == nullptr || result->generation != context_generation_ ||
        result->request_id != active_candidate_request_id_ ||
        result->page != candidate_page_ || result->expanded != candidates_expanded_ ||
        input_buffer_.empty()) return;
    if (result->request_failed && candidate_retry_count_ == 0) {
        ++candidate_retry_count_;
        schedule_candidate_request(false);
        return;
    }
    candidate_request_pending_ = false;
    candidate_request_failed_ = result->request_failed;
    candidate_failure_detail_ = std::move(result->failure_detail);
    if (!result->request_failed) candidate_retry_count_ = 0;
    candidates_ = std::move(result->candidates);
    candidate_consumed_ = std::move(result->candidate_consumed);
    if (candidate_consumed_.size() != candidates_.size()) {
        candidates_.clear();
        candidate_consumed_.clear();
    }
    if (!result->preserve_paging && result->page_size >= 1 && result->page_size <= 9)
        candidate_page_size_ = result->page_size;
    if (candidates_expanded_) {
        const auto page_size = std::max<std::size_t>(
            1, static_cast<std::size_t>(candidate_page_size_));
        const auto total_rows = (candidates_.size() + page_size - 1) / page_size;
        const auto maximum_scroll = total_rows > kExpandedVisibleRows
                                        ? total_rows - kExpandedVisibleRows
                                        : 0;
        expanded_scroll_row_ = std::min(expanded_scroll_row_, maximum_scroll);
    }
    if (!result->preserve_paging) {
        has_more_candidates_ = result->has_more;
        if (!result->segmented_input.empty())
            segmented_input_ = std::move(result->segmented_input);
    }
    if (deferred_candidate_text_) {
        const auto selected = std::find(candidates_.begin(), candidates_.end(),
                                        *deferred_candidate_text_);
        ITfContext* selected_context =
            std::exchange(deferred_candidate_context_, nullptr);
        deferred_candidate_text_.reset();
        if (selected != candidates_.end() && selected_context != nullptr) {
            const auto index = static_cast<std::size_t>(selected - candidates_.begin());
            const HRESULT committed = commit_candidate(selected_context, index);
            selected_context->Release();
            if (SUCCEEDED(committed)) return;
        } else if (selected_context != nullptr) {
            selected_context->Release();
        }
    }
    update_candidate_window();
}

void TextService::update_candidate_window() {
    if (candidate_window_ == nullptr || input_buffer_.empty() || !foreground_focus_) return;
    POINT position = candidate_anchor_;
    if (!candidate_anchor_valid_) GetCursorPos(&position);
    const UINT dpi = std::max(GetDpiForWindow(candidate_window_), 96U);
    SIZE window_size = desired_candidate_window_size();
    int x = position.x + (candidate_anchor_valid_ ? 0 : dips_to_pixels(12.0F, dpi));
    int y = position.y + (candidate_anchor_valid_ ? dips_to_pixels(6.0F, dpi)
                                                  : dips_to_pixels(20.0F, dpi));

    const HMONITOR monitor = MonitorFromPoint(position, MONITOR_DEFAULTTONEAREST);
    MONITORINFO monitor_info{sizeof(monitor_info)};
    if (GetMonitorInfoW(monitor, &monitor_info)) {
        const RECT& work_area = monitor_info.rcWork;
        const int available_width = static_cast<int>(work_area.right - work_area.left) -
                                    dips_to_pixels(16.0F, dpi);
        const int available_height = static_cast<int>(work_area.bottom - work_area.top) -
                                     dips_to_pixels(16.0F, dpi);
        window_size.cx = std::min(window_size.cx, static_cast<LONG>(available_width));
        window_size.cy = std::min(window_size.cy, static_cast<LONG>(available_height));
        x = std::min(x, static_cast<int>(work_area.right - window_size.cx));
        if (y + window_size.cy > work_area.bottom) {
            y = position.y - window_size.cy - dips_to_pixels(6.0F, dpi);
        }
        x = std::max(x, static_cast<int>(work_area.left));
        y = std::max(y, static_cast<int>(work_area.top));
    }
    RECT current{};
    GetWindowRect(candidate_window_, &current);
    const bool geometry_changed = current.left != x || current.top != y ||
                                  current.right - current.left != window_size.cx ||
                                  current.bottom - current.top != window_size.cy;
    if (geometry_changed || !IsWindowVisible(candidate_window_)) {
        SetWindowPos(candidate_window_, HWND_TOPMOST, x, y, window_size.cx, window_size.cy,
                     SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOCOPYBITS);
    }
    RedrawWindow(candidate_window_, nullptr, nullptr,
                 RDW_INVALIDATE | RDW_UPDATENOW | RDW_NOERASE | RDW_NOCHILDREN);
}

void TextService::change_candidate_page(const int direction) {
    if (candidate_request_pending_ || candidates_expanded_) return;
    if (direction > 0) {
        if (!has_more_candidates_) return;
        ++candidate_page_;
    } else if (direction < 0) {
        if (candidate_page_ == 0) return;
        --candidate_page_;
    } else {
        return;
    }
    has_more_candidates_ = false;
    candidates_expanded_ = false;
    hovered_target_.reset();
    pressed_target_.reset();
    queue_candidate_request();
}

void TextService::scroll_expanded_candidates(const int rows) {
    if (!candidates_expanded_ || rows == 0) return;
    const auto page_size = std::max<std::size_t>(
        1, static_cast<std::size_t>(candidate_page_size_));
    const auto total_rows = (candidates_.size() + page_size - 1) / page_size;
    const auto maximum_scroll = total_rows > kExpandedVisibleRows
                                    ? total_rows - kExpandedVisibleRows
                                    : 0;
    const auto current = static_cast<std::int64_t>(expanded_scroll_row_);
    const auto requested = current + static_cast<std::int64_t>(rows);
    const auto clamped = std::clamp<std::int64_t>(
        requested, 0, static_cast<std::int64_t>(maximum_scroll));
    if (clamped == current) return;
    expanded_scroll_row_ = static_cast<std::size_t>(clamped);
    hovered_target_.reset();
    pressed_target_.reset();
    update_candidate_window();
}

std::optional<TextService::HitTarget> TextService::hit_test(const POINT point) const {
    for (auto region = hit_regions_.rbegin(); region != hit_regions_.rend(); ++region) {
        if (PtInRect(&region->bounds, point)) return region->target;
    }
    return std::nullopt;
}

void TextService::invoke_hit_target(const HitTarget& target) {
    if (candidate_request_pending_) {
        if (target.kind == HitKind::candidate)
            defer_candidate_selection(target.candidate_index, nullptr);
        return;
    }
    switch (target.kind) {
        case HitKind::candidate:
            commit_candidate_from_window(target.candidate_index);
            break;
        case HitKind::previous_page:
            change_candidate_page(-1);
            break;
        case HitKind::next_page:
            change_candidate_page(1);
            break;
        case HitKind::toggle_expanded:
            if (!candidates_.empty() || candidates_expanded_) {
                const bool expanding = !candidates_expanded_;
                candidates_expanded_ = expanding;
                candidate_page_ = 0;
                has_more_candidates_ = false;
                expanded_scroll_row_ = 0;
                hovered_target_.reset();
                pressed_target_.reset();
                queue_candidate_request();
            }
            break;
    }
}

void TextService::defer_candidate_selection(const std::size_t index,
                                            ITfContext* context) {
    if (index >= candidates_.size()) return;
    ITfContext* selected_context = context;
    if (selected_context != nullptr) {
        selected_context->AddRef();
    } else if (thread_manager_ != nullptr) {
        ITfDocumentMgr* document_manager = nullptr;
        if (SUCCEEDED(thread_manager_->GetFocus(&document_manager)) &&
            document_manager != nullptr) {
            document_manager->GetTop(&selected_context);
            document_manager->Release();
        }
    }
    if (selected_context == nullptr) return;
    clear_deferred_candidate_selection();
    deferred_candidate_text_ = candidates_[index];
    deferred_candidate_context_ = selected_context;
}

void TextService::clear_deferred_candidate_selection() noexcept {
    deferred_candidate_text_.reset();
    release_interface(deferred_candidate_context_);
}

void TextService::update_candidate_anchor(ITfContext* context) {
    auto* session = new (std::nothrow)
        CaretEditSession(context, &candidate_anchor_, &candidate_anchor_valid_);
    if (session == nullptr) return;
    HRESULT session_result = E_FAIL;
    context->RequestEditSession(client_id_, session, TF_ES_SYNC | TF_ES_READ,
                                &session_result);
    session->Release();
}

void TextService::clear_composition() {
    ++context_generation_;
    input_buffer_.clear();
    segmented_input_.clear();
    candidates_.clear();
    candidate_consumed_.clear();
    candidate_failure_detail_.clear();
    hit_regions_.clear();
    hovered_target_.reset();
    pressed_target_.reset();
    clear_deferred_candidate_selection();
    candidate_page_ = 0;
    has_more_candidates_ = false;
    candidate_request_pending_ = false;
    candidate_request_failed_ = false;
    candidate_retry_count_ = 0;
    candidates_expanded_ = false;
    expanded_scroll_row_ = 0;
    candidate_anchor_valid_ = false;
    if (candidate_window_ != nullptr) ShowWindow(candidate_window_, SW_HIDE);
}

HRESULT TextService::commit_candidate(ITfContext* context, const std::size_t index) {
    if (candidate_request_pending_) return E_PENDING;
    if (index >= candidates_.size() || index >= candidate_consumed_.size())
        return E_INVALIDARG;
    const auto consumed = candidate_consumed_[index];
    if (consumed == 0 || consumed > input_buffer_.size()) return E_INVALIDARG;
    const std::wstring committed = candidates_[index];
    auto* session = new (std::nothrow) CommitEditSession(context, committed);
    if (session == nullptr) return E_OUTOFMEMORY;
    HRESULT session_result = E_FAIL;
    const HRESULT request_result = context->RequestEditSession(
        client_id_, session, TF_ES_SYNC | TF_ES_READWRITE, &session_result);
    session->Release();
    if (SUCCEEDED(request_result) && SUCCEEDED(session_result)) {
        if (consumed == input_buffer_.size()) {
            clear_composition();
        } else {
            input_buffer_.erase(0, static_cast<std::size_t>(consumed));
            while (!input_buffer_.empty() && input_buffer_.front() == L'\'')
                input_buffer_.erase(input_buffer_.begin());
            if (input_buffer_.empty()) {
                clear_composition();
            } else {
                ++context_generation_;
                segmented_input_.clear();
                candidate_page_ = 0;
                has_more_candidates_ = false;
                candidate_request_pending_ = false;
                candidates_expanded_ = false;
                hovered_target_.reset();
                pressed_target_.reset();
                update_candidate_anchor(context);
                queue_candidate_request();
            }
        }
        queue_commit_feedback(committed);
    }
    return FAILED(request_result) ? request_result : session_result;
}

HRESULT TextService::commit_raw_input(ITfContext* context) {
    if (context == nullptr || input_buffer_.empty()) return E_INVALIDARG;
    auto* session = new (std::nothrow) CommitEditSession(context, input_buffer_);
    if (session == nullptr) return E_OUTOFMEMORY;
    HRESULT session_result = E_FAIL;
    const HRESULT request_result = context->RequestEditSession(
        client_id_, session, TF_ES_SYNC | TF_ES_READWRITE, &session_result);
    session->Release();
    if (SUCCEEDED(request_result) && SUCCEEDED(session_result)) clear_composition();
    return FAILED(request_result) ? request_result : session_result;
}

HRESULT TextService::commit_candidate_from_window(const std::size_t index) {
    if (thread_manager_ == nullptr || index >= candidates_.size()) return E_INVALIDARG;
    ITfDocumentMgr* document_manager = nullptr;
    HRESULT result = thread_manager_->GetFocus(&document_manager);
    if (FAILED(result) || document_manager == nullptr)
        return FAILED(result) ? result : E_FAIL;
    ITfContext* context = nullptr;
    result = document_manager->GetTop(&context);
    document_manager->Release();
    if (FAILED(result) || context == nullptr) return FAILED(result) ? result : E_FAIL;
    result = commit_candidate(context, index);
    context->Release();
    return result;
}

HRESULT create_text_service(REFIID iid, void** object) {
    if (object == nullptr) return E_POINTER;
    *object = nullptr;
    auto* service = new (std::nothrow) TextService();
    if (service == nullptr) return E_OUTOFMEMORY;
    const HRESULT result = service->QueryInterface(iid, object);
    service->Release();
    return result;
}

void increment_server_lock() noexcept { InterlockedIncrement(&lock_count); }
void decrement_server_lock() noexcept { InterlockedDecrement(&lock_count); }
long server_lock_count() noexcept { return object_count + lock_count; }

}  // namespace owo::tsf
