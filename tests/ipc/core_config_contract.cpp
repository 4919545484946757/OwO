#include "owo/config/config_monitor.h"
#include "owo/config/config_store.h"
#include "owo/engine/lexicon.h"
#include "owo/engine/user_frequency.h"
#include "owo/ipc/named_pipe.h"
#include "owo/protocol/messages.h"

#include <chrono>
#include <filesystem>
#include <iostream>
#include <thread>

namespace {

owo::protocol::DecodeResult send(const std::wstring& pipe, const owo::protocol::Message& message) {
    for (int attempt = 0; attempt < 100; ++attempt) {
        const auto exchanged = owo::ipc::exchange(
            pipe.c_str(), owo::protocol::encode_message(message), std::chrono::milliseconds(100));
        if (exchanged.status) return owo::protocol::decode_message(exchanged.response);
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    return {};
}

bool acknowledged(const owo::protocol::DecodeResult& result, std::string_view text) {
    return result.validation &&
           result.message.type == owo::protocol::MessageType::acknowledgement &&
           result.message.text == text;
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) return 2;
    const std::filesystem::path root(argv[1]);
    std::error_code ignored;
    std::filesystem::remove_all(root, ignored);
    std::filesystem::create_directories(root, ignored);
    const auto config_path = root / "owo.conf";
    const auto frequency_path = root / "user.bin";

    owo::config::ConfigStore writer;
    if (!writer.load(config_path).success) return 2;
    auto config = writer.snapshot();
    config.candidate_page_size = 3;
    config.user_learning_enabled = false;
    if (!writer.save(config).success) return 2;

    owo::config::ConfigMonitor monitor;
    if (!monitor.start(config_path, std::chrono::milliseconds(10)).success) return 2;
    owo::engine::UserFrequencyStore frequencies;
    if (!frequencies.load(frequency_path).success) return 2;
    const owo::engine::MemoryLexicon lexicon({
        {{"ce", "shi"}, "测试一", 1000}, {{"ce", "shi"}, "测试二", 900},
        {{"ce", "shi"}, "测试三", 800}, {{"ce", "shi"}, "测试四", 700},
        {{"ce", "shi"}, "测试五", 600}, {{"ce", "shi"}, "测试六", 500},
        {{"ce", "shi"}, "测试七", 400}, {{"ni", "hao"}, "你好", 1000},
        {{"ni", "hao"}, "泥号", 2000}});
    const auto suffix = std::chrono::steady_clock::now().time_since_epoch().count();
    const auto pipe = LR"(\\.\pipe\OwO.InputMethod.ConfigContract.)" + std::to_wstring(suffix);
    const auto model_pipe = LR"(\\.\pipe\OwO.InputMethod.ConfigModelContract.)" +
                            std::to_wstring(suffix);

    int server_exit = -1;
    std::jthread server([&] {
        server_exit = owo::ipc::run_core_server(pipe.c_str(), lexicon, &frequencies,
                                                model_pipe.c_str(), &monitor);
    });
    const auto model_disabled = send(pipe, {owo::protocol::MessageType::candidate_request,
                                            10, 1, "nihao"});
    const auto page_of_three = send(pipe, {owo::protocol::MessageType::candidate_request,
                                           20, 1, "ceshi"});
    owo::protocol::Message expanded_request{
        owo::protocol::MessageType::candidate_request, 22, 1, "ceshi"};
    expanded_request.expanded = true;
    const auto expanded = send(pipe, expanded_request);
    const auto disabled = send(pipe, {owo::protocol::MessageType::candidate_committed,
                                      1, 1, "你好"});
    const auto generation = monitor.generation();
    config.user_learning_enabled = true;
    config.model_ranking_enabled = true;
    config.model_timeout_ms = 5;
    const bool enabled_reload = writer.save(config).success &&
        monitor.wait_for_generation(generation, std::chrono::seconds(2));
    owo::protocol::Message enabled_feedback{
        owo::protocol::MessageType::candidate_committed, 2, 1, "你好"};
    enabled_feedback.input = "NiHao";
    enabled_feedback.context = "我说";
    const auto enabled = send(pipe, enabled_feedback);
    owo::protocol::Message language_ranked_request{
        owo::protocol::MessageType::candidate_request, 13, 1, "nihao"};
    language_ranked_request.context = "我说";
    const auto language_ranked = send(pipe, language_ranked_request);
    const auto model_enabled = send(pipe, {owo::protocol::MessageType::candidate_request,
                                           11, 1, "nihao"});
    owo::protocol::Message smart_expanded_request{
        owo::protocol::MessageType::candidate_request, 14, 1, "nihao"};
    smart_expanded_request.expanded = true;
    const auto smart_expanded = send(pipe, smart_expanded_request);
    const auto generation_after_enable = monitor.generation();
    config.model_ranking_enabled = false;
    config.candidate_page_size = 2;
    const bool disabled_reload = writer.save(config).success &&
        monitor.wait_for_generation(generation_after_enable, std::chrono::seconds(2));
    const auto model_disabled_again = send(
        pipe, {owo::protocol::MessageType::candidate_request, 12, 1, "nihao"});
    owo::protocol::Message page_request{owo::protocol::MessageType::candidate_request,
                                        21, 1, "ceshi"};
    page_request.page = 1;
    const auto page_of_two = send(pipe, page_request);
    const auto shutdown = send(pipe, {owo::protocol::MessageType::shutdown_request,
                                      3, 1, {}});
    server.join();

    owo::engine::UserFrequencyStore persisted;
    const auto loaded = persisted.load(frequency_path);
    const bool ok = enabled_reload && disabled_reload &&
                    acknowledged(disabled, "commit_ack") &&
                    acknowledged(enabled, "commit_ack") &&
                    language_ranked.validation &&
                    !language_ranked.message.candidates.empty() &&
                    language_ranked.message.candidates.front() == "你好" &&
                    acknowledged(shutdown, "shutdown_ack") && server_exit == 0 &&
                    model_disabled.validation && !model_disabled.message.model_pending &&
                    page_of_three.validation && page_of_three.message.candidates.size() == 3 &&
                    page_of_three.message.page_size == 3 && page_of_three.message.has_more &&
                    expanded.validation && expanded.message.expanded &&
                    expanded.message.page_size == 3 && expanded.message.candidates.size() == 7 &&
                    !expanded.message.has_more &&
                    model_enabled.validation && model_enabled.message.model_pending &&
                    smart_expanded.validation && smart_expanded.message.expanded &&
                    smart_expanded.message.model_pending &&
                    model_disabled_again.validation &&
                    !model_disabled_again.message.model_pending &&
                    page_of_two.validation && page_of_two.message.page == 1 &&
                    page_of_two.message.candidates ==
                        std::vector<std::string>{"测试三", "测试四"} &&
                    page_of_two.message.has_more &&
                    loaded.success && persisted.count("你好") == 1 &&
                    persisted.contextual_count("nihao", "你好") == 1 &&
                    persisted.language_context_score("我说", "nihao", "你好") > 0;
    std::filesystem::remove_all(root, ignored);
    if (!ok) {
        std::cerr << "core hot configuration contract failed\n";
        return 1;
    }
    return 0;
}
