#include "owo/ipc/contextual_candidate_order.h"

#include <cstdint>
#include <string>
#include <vector>

int main() {
    std::vector<std::string> ordered;
    std::vector<std::uint64_t> consumed;
    owo::ipc::apply_contextual_candidate_order(
        {"问题", "工作", "世界", "问"}, 8,
        {"工作", "世界", "问题", "问"}, {8, 8, 8, 2}, ordered, consumed);
    if (ordered != std::vector<std::string>{"问题", "工作", "世界", "问"} ||
        consumed != std::vector<std::uint64_t>{8, 8, 8, 2}) return 1;

    owo::ipc::apply_contextual_candidate_order(
        {"字", "较长词", "另一整句", "整句"}, 10,
        {"整句", "较长词", "字", "另一整句"}, {10, 6, 2, 10}, ordered, consumed);
    if (ordered != std::vector<std::string>{"另一整句", "整句", "较长词", "字"} ||
        consumed != std::vector<std::uint64_t>{10, 10, 6, 2}) return 2;

    owo::ipc::apply_contextual_candidate_order(
        {"已知"}, 8, {"未知一", "未知二", "已知"}, {8, 8, 8}, ordered, consumed);
    if (ordered != std::vector<std::string>{"已知", "未知一", "未知二"}) return 3;
    return 0;
}
