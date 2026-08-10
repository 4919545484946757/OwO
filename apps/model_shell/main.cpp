#include "owo/ipc/named_pipe.h"
#include "owo/model/model_protocol.h"

#include <Windows.h>

#include <chrono>
#include <iostream>
#include <string>
#include <string_view>

namespace {
std::string utf8(const std::wstring_view value) {
    if (value.empty()) return {};
    const int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                         static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
    if (size <= 0) return {};
    std::string output(static_cast<std::size_t>(size), '\0');
    if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                            static_cast<int>(value.size()), output.data(), size,
                            nullptr, nullptr) != size) return {};
    return output;
}
}  // namespace

int wmain(int argc, wchar_t** argv) {
    owo::model::ModelMessage request;
    request.request_id = 1;
    request.status = owo::model::ModelStatus::success;
    int input_index = 1;
    while (input_index + 1 < argc) {
        const std::wstring_view option(argv[input_index]);
        if (option == L"--model-id") {
            request.model_id = utf8(argv[input_index + 1]);
            input_index += 2;
        } else if (option == L"--context") {
            request.context = utf8(argv[input_index + 1]);
            input_index += 2;
        } else {
            break;
        }
    }
    if (argc == 2 && std::wstring_view(argv[1]) == L"--shutdown") {
        request.type = owo::model::ModelMessageType::shutdown_request;
    } else if (argc - input_index >= 2) {
        request.type = owo::model::ModelMessageType::rank_request;
        request.timeout_ms = 100;
        request.input = utf8(argv[input_index]);
        for (int index = input_index + 1; index < argc; ++index)
            request.candidates.push_back(utf8(argv[index]));
    } else {
        std::cerr << "usage: owo_model_shell [--model-id <id>] [--context <text>] "
                     "<input> <candidate> [...] | --shutdown\n";
        return 2;
    }
    const auto exchanged = owo::ipc::exchange(
        owo::ipc::kModelHostPipeName, owo::model::encode_model_message(request),
        std::chrono::milliseconds(500));
    if (!exchanged.status) {
        std::cerr << exchanged.status.message << '\n';
        return 3;
    }
    const auto response = owo::model::decode_model_message(exchanged.response);
    if (!response.validation) return 4;
    if (response.message.type == owo::model::ModelMessageType::acknowledgement) {
        std::cout << "shutdown_ack\n";
        return 0;
    }
    if (response.message.type != owo::model::ModelMessageType::rank_response ||
        response.message.status != owo::model::ModelStatus::success) {
        std::cerr << response.message.diagnostic << '\n';
        return 5;
    }
    for (std::size_t index = 0; index < response.message.candidates.size(); ++index)
        std::cout << index + 1 << ". " << response.message.candidates[index] << '\n';
    return 0;
}
