#include "owo/core/plugin_executor.h"
#include "owo/plugin/plugin_services.h"

#include "owo/plugin/plugin_host.h"

#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <deque>
#include <iostream>
#include <mutex>
#include <thread>
#include <utility>

namespace owo::core {
namespace {

const char* source_name(const PluginCallSource source) {
    switch (source) {
    case PluginCallSource::tsf_input: return "tsf_input";
    case PluginCallSource::core_background: return "core_background";
    case PluginCallSource::trusted_user_action: return "trusted_user_action";
    case PluginCallSource::plugin_service: return "plugin_service";
    }
    return "unknown";
}

const char* status_name(const PluginExecutionStatus status) {
    switch (status) {
    case PluginExecutionStatus::success: return "success";
    case PluginExecutionStatus::invalid_request: return "invalid_request";
    case PluginExecutionStatus::rejected_source: return "rejected_source";
    case PluginExecutionStatus::queue_full: return "queue_full";
    case PluginExecutionStatus::cancelled: return "cancelled";
    case PluginExecutionStatus::timeout: return "timeout";
    case PluginExecutionStatus::launch_failed: return "launch_failed";
    case PluginExecutionStatus::invocation_failed: return "invocation_failed";
    case PluginExecutionStatus::stopped: return "stopped";
    }
    return "unknown";
}

std::string json_escape(const std::string_view value) {
    static constexpr char digits[] = "0123456789abcdef";
    std::string result;
    result.reserve(value.size() + 2);
    result.push_back('"');
    for (const unsigned char byte : value) {
        if (byte == '"') result += "\\\"";
        else if (byte == '\\') result += "\\\\";
        else if (byte < 0x20) {
            result += "\\u00";
            result.push_back(digits[byte >> 4]);
            result.push_back(digits[byte & 0x0f]);
        } else result.push_back(static_cast<char>(byte));
    }
    result.push_back('"');
    return result;
}

void audit(const std::uint64_t call_id, const PluginCallSource source,
           const std::string_view plugin_id, const std::string_view service,
           const PluginExecutionStatus status) {
    static std::mutex log_mutex;
    std::lock_guard lock(log_mutex);
    std::clog << R"({"process":"core_service","module":"plugin","level":"info","event_id":"plugin_call","call_id":)"
              << call_id << R"(,"source":)" << json_escape(source_name(source))
              << R"(,"plugin_id":)" << json_escape(plugin_id)
              << R"(,"service":)" << json_escape(service)
              << R"(,"status":)" << json_escape(status_name(status)) << "}\n";
}

bool plugin_id_text(const std::string_view value) {
    if (value.size() < 3 || value.size() > 128 || value.front() == '.' ||
        value.back() == '.' || value.find('.') == std::string_view::npos) return false;
    bool previous_dot = false;
    for (const unsigned char byte : value) {
        const bool valid = (byte >= 'a' && byte <= 'z') || (byte >= '0' && byte <= '9') ||
                           byte == '-' || byte == '.';
        if (!valid || (byte == '.' && previous_dot)) return false;
        previous_dot = byte == '.';
    }
    return true;
}

bool versioned_service(const std::string_view service) {
    constexpr std::string_view suffix = ".v1";
    if (service.size() <= suffix.size() || service.size() > 128 || !service.ends_with(suffix))
        return false;
    return std::all_of(service.begin(), service.end(), [](const unsigned char byte) {
        return (byte >= 'a' && byte <= 'z') || (byte >= '0' && byte <= '9') ||
               byte == '.' || byte == '-' || byte == '_';
    });
}

bool valid_source(const PluginCallSource source) {
    switch (source) {
    case PluginCallSource::tsf_input:
    case PluginCallSource::core_background:
    case PluginCallSource::trusted_user_action:
    case PluginCallSource::plugin_service:
        return true;
    }
    return false;
}

PluginExecutionResult result(const std::uint64_t call_id,
                             const PluginExecutionStatus status,
                             std::string diagnostic = {}) {
    return {status == PluginExecutionStatus::success, call_id, status,
            plugin::PluginStatus::plugin_error, false, {}, std::move(diagnostic)};
}

}  // namespace

struct PluginExecutor::Impl {
    struct Job {
        std::uint64_t call_id{};
        PluginExecutionRequest request;
        std::chrono::steady_clock::time_point deadline;
        std::promise<PluginExecutionResult> promise;
    };

    explicit Impl(std::filesystem::path root) : store_root(std::move(root)),
        worker([this](const std::stop_token stop) { run(stop); }) {}

    ~Impl() {
        worker.request_stop();
        condition.notify_all();
        if (worker.joinable()) worker.join();
    }

    std::uint64_t allocate_call_id() noexcept {
        auto value = next_call_id.fetch_add(1, std::memory_order_relaxed);
        if (value != 0) return value;
        return next_call_id.fetch_add(1, std::memory_order_relaxed);
    }

    void finish(Job& job, PluginExecutionResult value) {
        audit(job.call_id, job.request.source, job.request.plugin_id,
              job.request.service, value.status);
        job.promise.set_value(std::move(value));
    }

    void run(const std::stop_token stop) {
        while (true) {
            Job job;
            {
                std::unique_lock lock(mutex);
                condition.wait(lock, stop, [&] { return !queue.empty(); });
                if (stop.stop_requested()) break;
                job = std::move(queue.front());
                queue.pop_front();
            }
            if (job.request.cancellation.stop_requested()) {
                finish(job, result(job.call_id, PluginExecutionStatus::cancelled,
                                   "plugin call was cancelled before worker dispatch"));
                continue;
            }
            auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
                job.deadline - std::chrono::steady_clock::now());
            if (remaining.count() <= 0) {
                finish(job, result(job.call_id, PluginExecutionStatus::timeout,
                                   "plugin call expired in the worker queue"));
                continue;
            }
            if (!session.valid() || session.plugin_id() != job.request.plugin_id) {
                if (session.valid()) {
                    const auto stopped = session.shutdown(std::chrono::seconds(1));
                    if (!stopped.ok) session.terminate();
                }
                const auto startup_timeout = (std::min)(
                    remaining, std::chrono::duration_cast<std::chrono::milliseconds>(
                                   std::chrono::seconds(5)));
                auto launched = plugin::launch_active_plugin(
                    store_root, job.request.plugin_id, startup_timeout);
                if (!launched) {
                    finish(job, result(job.call_id, PluginExecutionStatus::launch_failed,
                                       std::move(launched.diagnostic)));
                    continue;
                }
                session = std::move(launched.session);
            }
            remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
                job.deadline - std::chrono::steady_clock::now());
            if (remaining.count() <= 0) {
                finish(job, result(job.call_id, PluginExecutionStatus::timeout,
                                   "plugin call expired during plugin startup"));
                continue;
            }
            std::stop_source combined;
            std::stop_callback request_cancel(job.request.cancellation,
                [&combined] { combined.request_stop(); });
            std::stop_callback worker_cancel(stop, [&combined] { combined.request_stop(); });
            plugin::PluginInvokeRequest request;
            request.service = job.request.service;
            request.payload = std::move(job.request.payload);
            request.required_permissions = std::move(job.request.required_permissions);
            request.user_initiated = job.request.source == PluginCallSource::trusted_user_action;
            request.timeout = remaining;
            request.cancellation = combined.get_token();
            auto invoked = session.invoke(std::move(request));
            PluginExecutionResult completed;
            completed.ok = invoked.ok;
            completed.call_id = job.call_id;
            completed.status = invoked.ok ? PluginExecutionStatus::success
                : invoked.status == plugin::PluginStatus::cancelled
                    ? PluginExecutionStatus::cancelled
                : invoked.status == plugin::PluginStatus::timeout
                    ? PluginExecutionStatus::timeout
                    : PluginExecutionStatus::invocation_failed;
            completed.plugin_status = invoked.status;
            completed.forced_termination = invoked.forced_termination;
            completed.payload = std::move(invoked.payload);
            completed.diagnostic = std::move(invoked.diagnostic);
            finish(job, std::move(completed));
        }
        if (session.valid()) {
            const auto stopped = session.shutdown(std::chrono::seconds(1));
            if (!stopped.ok) session.terminate();
        }
        std::deque<Job> abandoned;
        {
            std::lock_guard lock(mutex);
            abandoned.swap(queue);
        }
        for (auto& job : abandoned)
            finish(job, result(job.call_id, PluginExecutionStatus::stopped,
                               "plugin executor stopped before dispatch"));
    }

    std::filesystem::path store_root;
    std::atomic<std::uint64_t> next_call_id{1};
    mutable std::mutex mutex;
    std::condition_variable_any condition;
    std::deque<Job> queue;
    plugin::PluginHostSession session;
    std::jthread worker;
};

PluginExecutor::PluginExecutor(std::filesystem::path plugin_store_root)
    : implementation_(std::make_unique<Impl>(std::move(plugin_store_root))) {}

PluginExecutor::~PluginExecutor() = default;

PluginExecutionSubmission PluginExecutor::submit(PluginExecutionRequest request) {
    const auto call_id = implementation_->allocate_call_id();
    Impl::Job job;
    job.call_id = call_id;
    job.deadline = std::chrono::steady_clock::now() + request.timeout;
    job.request = std::move(request);
    if (const auto* service = plugin::find_plugin_service(job.request.service);
        service != nullptr && !service->required_permission.empty() &&
        std::find(job.request.required_permissions.begin(),
                  job.request.required_permissions.end(), service->required_permission) ==
            job.request.required_permissions.end()) {
        job.request.required_permissions.emplace_back(service->required_permission);
    }
    auto completion = job.promise.get_future();
    auto reject = [&](const PluginExecutionStatus status, std::string diagnostic) {
        audit(call_id, job.request.source, job.request.plugin_id, job.request.service, status);
        job.promise.set_value(result(call_id, status, std::move(diagnostic)));
        return PluginExecutionSubmission{call_id, std::move(completion)};
    };
    if (!plugin_id_text(job.request.plugin_id) || !versioned_service(job.request.service) ||
        !valid_source(job.request.source) ||
        job.request.payload.size() > plugin::kMaximumPluginPayloadBytes ||
        job.request.timeout.count() <= 0 ||
        job.request.timeout > plugin::kMaximumPluginInvocationTimeout ||
        job.request.call_depth > kMaximumPluginCallDepth)
        return reject(PluginExecutionStatus::invalid_request,
                      "invalid bounded Core plugin call request");
    if (job.request.source == PluginCallSource::tsf_input)
        return reject(PluginExecutionStatus::rejected_source,
                      "TSF input requests cannot invoke plugins");
    if (!job.request.required_permissions.empty() &&
        job.request.source != PluginCallSource::trusted_user_action)
        return reject(PluginExecutionStatus::rejected_source,
                      "sensitive plugin calls require a trusted user-action source");
    if (job.request.cancellation.stop_requested())
        return reject(PluginExecutionStatus::cancelled,
                      "plugin call was cancelled before queueing");
    {
        std::lock_guard lock(implementation_->mutex);
        if (implementation_->worker.get_stop_token().stop_requested())
            return reject(PluginExecutionStatus::stopped, "plugin executor is stopping");
        if (implementation_->queue.size() >= kMaximumQueuedPluginCalls)
            return reject(PluginExecutionStatus::queue_full, "plugin call queue is full");
        implementation_->queue.push_back(std::move(job));
    }
    implementation_->condition.notify_one();
    return {call_id, std::move(completion)};
}

std::size_t PluginExecutor::queued_call_count() const {
    std::lock_guard lock(implementation_->mutex);
    return implementation_->queue.size();
}

}  // namespace owo::core
