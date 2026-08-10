#pragma once

#include <chrono>
#include <cstdint>
#include <future>
#include <memory>
#include <stop_token>
#include <string>
#include <string_view>
#include <vector>

namespace owo::model {

enum class ModelStatus { success, cancelled, timeout, backend_error };

struct ModelRequest {
    std::uint64_t request_id{};
    std::string model_id;
    std::string input;
    std::vector<std::string> candidates;
    std::chrono::milliseconds timeout{100};
    std::string context;
};

struct ModelResult {
    std::uint64_t request_id{};
    ModelStatus status{ModelStatus::backend_error};
    std::vector<std::string> candidates;
    std::string diagnostic;
};

class ICandidateRanker {
public:
    virtual ~ICandidateRanker() = default;
    [[nodiscard]] virtual ModelResult rank(const ModelRequest&, std::stop_token) = 0;
};

class ITextCompletionModel {
public:
    virtual ~ITextCompletionModel() = default;
    [[nodiscard]] virtual ModelResult complete(const ModelRequest&, std::stop_token) = 0;
};

class IAdvancedReranker : public ICandidateRanker {
public:
    ~IAdvancedReranker() override = default;
};

class IModelBackend : public ICandidateRanker {
public:
    ~IModelBackend() override = default;
    [[nodiscard]] virtual std::string_view id() const noexcept = 0;
};

class IModelScheduler {
public:
    virtual ~IModelScheduler() = default;
    [[nodiscard]] virtual std::future<ModelResult> submit(ModelRequest) = 0;
    virtual bool cancel(std::uint64_t request_id) = 0;
};

struct MockBackendOptions {
    std::chrono::milliseconds latency{};
    bool fail{};
};

class MockModelBackend final : public IModelBackend {
public:
    explicit MockModelBackend(MockBackendOptions options = {}) : options_(options) {}
    [[nodiscard]] std::string_view id() const noexcept override { return "owo.mock.rank.v1"; }
    [[nodiscard]] ModelResult rank(const ModelRequest&, std::stop_token) override;

private:
    MockBackendOptions options_;
};

struct LibimeBackendLoadResult {
    std::unique_ptr<IModelBackend> backend;
    std::string diagnostic;

    [[nodiscard]] explicit operator bool() const noexcept { return backend != nullptr; }
};

[[nodiscard]] LibimeBackendLoadResult load_libime_backend(std::string_view bridge_path,
                                                          std::string_view model_path);

class AsyncModelScheduler final : public IModelScheduler {
public:
    explicit AsyncModelScheduler(std::shared_ptr<IModelBackend> backend);
    [[nodiscard]] std::future<ModelResult> submit(ModelRequest request) override;
    bool cancel(std::uint64_t request_id) override;

private:
    struct State;
    std::shared_ptr<IModelBackend> backend_;
    std::shared_ptr<State> state_;
};

}  // namespace owo::model
