#include "owo/model/model_backend.h"

#include <chrono>
#include <memory>
#include <thread>

using namespace std::chrono_literals;

int main() {
    const auto missing_libime =
        owo::model::load_libime_backend("missing-owo-libime-bridge.dll", "missing.zh_CN.lm");
    if (missing_libime || missing_libime.diagnostic.empty()) return 1;

    auto backend = std::make_shared<owo::model::MockModelBackend>();
    owo::model::AsyncModelScheduler scheduler(backend);
    auto success = scheduler.submit({1, "owo.mock.rank.v1", "sensitive-input", {"泥号", "你好"}, 100ms});
    const auto ranked = success.get();
    if (ranked.status != owo::model::ModelStatus::success ||
        ranked.candidates != std::vector<std::string>{"你好", "泥号"}) return 1;

    auto slow = std::make_shared<owo::model::MockModelBackend>(
        owo::model::MockBackendOptions{100ms, false});
    owo::model::AsyncModelScheduler cancellable(slow);
    auto cancelled = cancellable.submit({2, "owo.mock.rank.v1", {}, {"b", "a"}, 500ms});
    std::this_thread::sleep_for(5ms);
    if (!cancellable.cancel(2) || cancelled.get().status != owo::model::ModelStatus::cancelled) return 1;

    owo::model::AsyncModelScheduler expiring(slow);
    auto timeout = expiring.submit({3, "owo.mock.rank.v1", {}, {"b", "a"}, 5ms});
    if (timeout.get().status != owo::model::ModelStatus::timeout) return 1;

    auto failing_backend = std::make_shared<owo::model::MockModelBackend>(
        owo::model::MockBackendOptions{0ms, true});
    owo::model::AsyncModelScheduler failing(failing_backend);
    if (failing.submit({4, "owo.mock.rank.v1", {}, {"b", "a"}, 100ms}).get().status !=
        owo::model::ModelStatus::backend_error) return 1;
    return 0;
}
