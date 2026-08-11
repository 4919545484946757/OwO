#include "owo/ipc/named_pipe.h"
#include "owo/engine/binary_lexicon.h"
#include "owo/engine/user_frequency.h"
#include "owo/protocol/messages.h"

#include <chrono>
#include <atomic>
#include <iostream>
#include <filesystem>
#include <thread>

namespace {

std::wstring contract_pipe_name() {
    const auto unique_suffix = std::chrono::steady_clock::now().time_since_epoch().count();
    return LR"(\\.\pipe\OwO.InputMethod.ContractTest.)" + std::to_wstring(unique_suffix);
}

owo::protocol::DecodeResult send_request(const std::wstring& pipe_name,
                                         const owo::protocol::Message& request) {
    for (int attempt = 0; attempt < 100; ++attempt) {
        const auto result = owo::ipc::exchange(pipe_name.c_str(),
                                               owo::protocol::encode_message(request),
                                               std::chrono::milliseconds(100));
        if (result.status) return owo::protocol::decode_message(result.response);
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    return {};
}

bool valid_response(const owo::protocol::DecodeResult& response,
                    const std::uint64_t request_id,
                    const std::uint64_t generation,
                    const std::vector<std::string>& candidates = {"你好", "你号"},
                    const std::vector<std::string>& syllables = {"ni", "hao"},
                    const std::uint64_t consumed = 5) {
    return response.validation &&
           response.message.type == owo::protocol::MessageType::candidate_response &&
           response.message.request_id == request_id &&
           response.message.context_generation == generation &&
           response.message.candidates == candidates &&
           response.message.syllables == syllables &&
           response.message.page_size == 5 && !response.message.expanded &&
           response.message.correction_enabled &&
           response.message.candidate_consumed ==
               std::vector<std::uint64_t>(candidates.size(), consumed);
}

}  // namespace

int main() {
    const auto pipe_name = contract_pipe_name();
    const auto path = std::filesystem::temp_directory_path() / "owo-core-contract.owolx";
    const auto user_path = std::filesystem::temp_directory_path() / "owo-core-contract-user.bin";
    std::error_code ignored;
    std::filesystem::remove(user_path, ignored);
    std::filesystem::remove(user_path.wstring() + L".bak", ignored);
    const auto written = owo::engine::write_binary_lexicon(path, {
        {{"ni", "hao"}, "你好", 1000}, {{"ni", "hao"}, "你号", 50},
        {{"ce", "shi"}, "测试一", 1000}, {{"ce", "shi"}, "测试二", 900},
        {{"ce", "shi"}, "测试三", 800}, {{"ce", "shi"}, "测试四", 700},
        {{"ce", "shi"}, "测试五", 600}, {{"ce", "shi"}, "测试六", 500},
        {{"ce", "shi"}, "测试七", 400},
        {{"wo", "ai"}, "我爱", 1500}, {{"wo"}, "我", 1400},
        {{"shi", "jie"}, "世界", 1600},
        {{"ga"}, "A", 1000}, {{"ge"}, "B", 900},
        {{"gou"}, "C", 800}, {{"gu"}, "D", 700},
        {{"gang"}, "E", 600}, {{"gong"}, "F", 500},
        {{"ga", "da"}, "aa", 2000}, {{"ge", "de"}, "bb", 1900},
        {{"gou", "dong"}, "cc", 1800}, {{"gu", "dian"}, "dd", 1700},
        {{"gang", "du"}, "ee", 1600}, {{"gong", "di"}, "ff", 1500},
        {{"gui", "dao"}, "gg", 1400}});
    owo::engine::BinaryLexicon lexicon;
    const auto loaded = lexicon.load(path);
    owo::engine::UserFrequencyStore user_frequency;
    if (!written.success || !loaded.success || !user_frequency.load(user_path).success) return 2;
    std::atomic<int> server_exit{-1};
    std::jthread server([&server_exit, &lexicon, &user_frequency, &pipe_name] {
        server_exit = owo::ipc::run_core_server(pipe_name.c_str(), lexicon, &user_frequency);
    });
    owo::ipc::PersistentPipeClient persistent(pipe_name);
    const auto first = owo::protocol::decode_message(persistent.exchange(
        owo::protocol::encode_message({owo::protocol::MessageType::candidate_request,
                                       101, 7, "nihao"}),
        std::chrono::seconds(2)).response);
    const auto second = owo::protocol::decode_message(persistent.exchange(
        owo::protocol::encode_message({owo::protocol::MessageType::candidate_request,
                                       102, 8, "nihao"}),
        std::chrono::seconds(2)).response);
    persistent.reset();
    const auto nihao_ranged = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request,
                    108, 8, "ni'hao'shi'jie"});
    const auto corrected = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request, 110, 8, "niaho"});
    const auto stable_xingb = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request, 112, 8, "xingb"});
    const auto stable_xingbaf = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request, 113, 8, "xingbaf"});
    const auto stable_mingd = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request, 116, 8, "mingd"});
    const auto stable_kenengd = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request, 117, 8, "kenengd"});
    const auto double_initial_first = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request, 118, 8, "gd"});
    owo::protocol::Message double_initial_second_request{
        owo::protocol::MessageType::candidate_request, 119, 8, "gd"};
    double_initial_second_request.page = 1;
    const auto double_initial_second = send_request(
        pipe_name, double_initial_second_request);
    owo::protocol::Message correction_disabled_request{
        owo::protocol::MessageType::candidate_request, 111, 8, "niaho"};
    correction_disabled_request.correction_enabled = false;
    const auto correction_disabled = send_request(pipe_name, correction_disabled_request);
    owo::protocol::Message separated_correction_disabled_request{
        owo::protocol::MessageType::candidate_request, 114, 8, "quan'loi"};
    separated_correction_disabled_request.correction_enabled = false;
    const auto separated_correction_disabled = send_request(
        pipe_name, separated_correction_disabled_request);
    const auto separated_correction_enabled = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request, 115, 8, "quan'loi"});
    bool commits_ok = true;
    for (std::uint64_t request = 0; request < 5; ++request) {
        const auto committed = send_request(pipe_name, {owo::protocol::MessageType::candidate_committed,
                                         200 + request, 8, "你号"});
        commits_ok = commits_ok && committed.validation &&
                     committed.message.type == owo::protocol::MessageType::acknowledgement;
    }
    const auto learned = send_request(pipe_name, {owo::protocol::MessageType::candidate_request, 104, 8, "nihao"});
    const auto first_page = send_request(pipe_name, {owo::protocol::MessageType::candidate_request, 106, 8, "ceshi"});
    owo::protocol::Message second_page_request{
        owo::protocol::MessageType::candidate_request, 105, 8, "ceshi"};
    second_page_request.page = 1;
    const auto second_page = send_request(pipe_name, second_page_request);
    owo::protocol::Message expanded_request{
        owo::protocol::MessageType::candidate_request, 109, 8, "ceshi"};
    expanded_request.expanded = true;
    const auto expanded = send_request(pipe_name, expanded_request);
    const auto ranged = send_request(
        pipe_name, {owo::protocol::MessageType::candidate_request,
                    107, 8, "wo'ai'shi'jie"});
    const auto shutdown = send_request(pipe_name, {owo::protocol::MessageType::shutdown_request, 103, 9, {}});
    server.join();
    std::filesystem::remove(path, ignored);
    owo::engine::UserFrequencyStore persisted;
    const auto persisted_result = persisted.load(user_path);
    std::filesystem::remove(user_path, ignored);
    std::filesystem::remove(user_path.wstring() + L".bak", ignored);
    if (!commits_ok || !valid_response(first, 101, 7) || !valid_response(second, 102, 8) ||
        !valid_response(learned, 104, 8, {"你号", "你好"}) ||
        !nihao_ranged.validation ||
        nihao_ranged.message.candidates !=
            std::vector<std::string>{"你好世界", "你好", "你号", "你号世界"} ||
        nihao_ranged.message.syllables !=
            std::vector<std::string>{"ni", "hao", "shi", "jie"} ||
        nihao_ranged.message.candidate_consumed !=
            std::vector<std::uint64_t>{14, 6, 6, 14} ||
        !valid_response(corrected, 110, 8, {"你好", "你号"}, {"ni", "aho"}) ||
        !stable_xingb.validation ||
        stable_xingb.message.syllables != std::vector<std::string>{"xing", "b"} ||
        !stable_xingbaf.validation ||
        stable_xingbaf.message.syllables !=
            std::vector<std::string>{"xing", "ba", "f"} ||
        !stable_mingd.validation ||
        stable_mingd.message.syllables != std::vector<std::string>{"ming", "d"} ||
        !stable_kenengd.validation ||
        stable_kenengd.message.syllables !=
            std::vector<std::string>{"ke", "neng", "d"} ||
        !double_initial_first.validation ||
        double_initial_first.message.candidate_consumed !=
            std::vector<std::uint64_t>{2, 2, 2, 1, 1} ||
        !double_initial_second.validation ||
        double_initial_second.message.candidate_consumed !=
            std::vector<std::uint64_t>{2, 2, 2, 1, 1} ||
        !correction_disabled.validation ||
        correction_disabled.message.type != owo::protocol::MessageType::candidate_response ||
        correction_disabled.message.correction_enabled ||
        !correction_disabled.message.candidates.empty() ||
        !separated_correction_disabled.validation ||
        separated_correction_disabled.message.type !=
            owo::protocol::MessageType::candidate_response ||
        separated_correction_disabled.message.correction_enabled ||
        !separated_correction_disabled.message.candidates.empty() ||
        separated_correction_disabled.message.syllables !=
            std::vector<std::string>{"quan", "loi"} ||
        !separated_correction_enabled.validation ||
        separated_correction_enabled.message.type !=
            owo::protocol::MessageType::candidate_response ||
        !separated_correction_enabled.message.correction_enabled ||
        !separated_correction_enabled.message.candidates.empty() ||
        separated_correction_enabled.message.syllables !=
            std::vector<std::string>{"quan", "loi"} ||
        !valid_response(first_page, 106, 8,
                        {"测试一", "测试二", "测试三", "测试四", "测试五"},
                        {"ce", "shi"}) ||
        first_page.message.page != 0 || !first_page.message.has_more ||
        !valid_response(second_page, 105, 8, {"测试六", "测试七"}, {"ce", "shi"}) ||
        second_page.message.page != 1 || second_page.message.has_more ||
        !expanded.validation || !expanded.message.expanded ||
        expanded.message.page != 0 || expanded.message.page_size != 5 ||
        expanded.message.has_more ||
        expanded.message.candidates !=
            std::vector<std::string>{"测试一", "测试二", "测试三", "测试四",
                                     "测试五", "测试六", "测试七"} ||
        expanded.message.candidate_consumed !=
            std::vector<std::uint64_t>(7, 5) ||
        !ranged.validation ||
        ranged.message.candidates != std::vector<std::string>{"我爱世界", "我爱", "我"} ||
        ranged.message.syllables !=
            std::vector<std::string>{"wo", "ai", "shi", "jie"} ||
        ranged.message.candidate_consumed !=
            std::vector<std::uint64_t>{13, 5, 2} ||
        !persisted_result.success || persisted.count("你号") != 5 ||
        !shutdown.validation || shutdown.message.type != owo::protocol::MessageType::acknowledgement ||
        shutdown.message.text != "shutdown_ack" ||
        shutdown.message.request_id != 103 || shutdown.message.context_generation != 9 ||
        server_exit != 0) {
        std::cerr << "in-process core service contract failed\n";
        return 1;
    }
    return 0;
}
