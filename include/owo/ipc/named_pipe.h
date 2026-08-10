#pragma once

#include "owo/protocol/envelope.h"

#include <chrono>
#include <cstdint>
#include <string>
#include <string_view>

namespace owo::engine {
class Lexicon;
class UserFrequencyStore;
}

namespace owo::config {
class ConfigMonitor;
}

namespace owo::model {
class IModelBackend;
}

namespace owo::ipc {

inline constexpr wchar_t kCorePipeName[] = LR"(\\.\pipe\OwO.InputMethod.Core.P1)";
inline constexpr wchar_t kModelHostPipeName[] = LR"(\\.\pipe\OwO.InputMethod.ModelHost.P3.v1)";

[[nodiscard]] inline std::wstring candidate_cancellation_event_name(
    const std::uint64_t request_id, const std::uint64_t generation) {
    return L"Local\\OwO.InputMethod.CandidateCancel.v1." +
           std::to_wstring(generation) + L"." + std::to_wstring(request_id);
}

struct ExchangeResult {
    protocol::ValidationResult status;
    std::string response;
};

class PersistentPipeClient final {
public:
    explicit PersistentPipeClient(std::wstring pipe_name);
    PersistentPipeClient(const PersistentPipeClient&) = delete;
    PersistentPipeClient& operator=(const PersistentPipeClient&) = delete;
    ~PersistentPipeClient();

    [[nodiscard]] ExchangeResult exchange(
        std::string_view request, std::chrono::milliseconds timeout);
    void reset() noexcept;

private:
    std::wstring pipe_name_;
    void* pipe_{};
};

/// 通过命名管道发送单个请求并等待单个响应。
/// @thread_safety 可并发调用；每次调用使用独立句柄。
[[nodiscard]] ExchangeResult exchange(
    const wchar_t* pipe_name,
    std::string_view request,
    std::chrono::milliseconds timeout);

/// 运行单连接串行服务循环，使用内置开发降级词典。
[[nodiscard]] int run_core_server(const wchar_t* pipe_name);

/// 运行使用显式词典的服务循环。词典在服务退出前必须保持有效。
[[nodiscard]] int run_core_server(const wchar_t* pipe_name, const engine::Lexicon& lexicon);
[[nodiscard]] int run_core_server(const wchar_t* pipe_name,
                                  const engine::Lexicon& lexicon,
                                  engine::UserFrequencyStore* user_frequency);
[[nodiscard]] int run_core_server(const wchar_t* pipe_name,
                                  const engine::Lexicon& lexicon,
                                  engine::UserFrequencyStore* user_frequency,
                                  const wchar_t* model_pipe_name);
[[nodiscard]] int run_core_server(const wchar_t* pipe_name,
                                  const engine::Lexicon& lexicon,
                                  engine::UserFrequencyStore* user_frequency,
                                  const wchar_t* model_pipe_name,
                                  const config::ConfigMonitor* config_monitor);

/// 运行 ModelHost v1 串行服务循环。后端在服务退出前必须保持有效。
[[nodiscard]] int run_model_server(const wchar_t* pipe_name, model::IModelBackend& backend);

}  // namespace owo::ipc
