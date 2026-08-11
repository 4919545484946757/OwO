#include "owo/plugin/plugin_manifest.h"

#include <string>

namespace {

const std::string valid = R"({
  "id":"owo.plugin.example",
  "name":"Example",
  "version":"1.0.0",
  "api_version":1,
  "runtime":"process",
  "entry":"bin/example.exe",
  "permissions":[],
  "network":false,
  "config_schema":"config.schema.json"
})";

bool rejected(std::string text, const std::string& from, const std::string& to) {
    const auto position = text.find(from);
    if (position == std::string::npos) return false;
    text.replace(position, from.size(), to);
    return !owo::plugin::parse_manifest(text).ok;
}

}  // namespace

int main(const int argc, char** argv) {
    const auto parsed = owo::plugin::parse_manifest(valid);
    if (!parsed.ok || parsed.value.id != "owo.plugin.example" || parsed.value.api_version != 1) return 1;
    if (!rejected(valid, "\"api_version\":1", "\"api_version\":2")) return 2;
    if (!rejected(valid, "\"runtime\":\"process\"", "\"runtime\":\"builtin\"")) return 3;
    if (!rejected(valid, "bin/example.exe", "../evil.exe")) return 4;
    if (!rejected(valid, "bin/example.exe", "bin//evil.exe")) return 13;
    if (!rejected(valid, "bin/example.exe", "C:/evil.exe")) return 14;
    if (!rejected(valid, "\"network\":false", "\"network\":true")) return 5;
    if (!rejected(valid, "\"permissions\":[]", "\"permissions\":[\"network\"]")) return 6;
    if (!rejected(valid, "\"permissions\":[]", "\"permissions\":[\"clipboard.read\",\"clipboard.read\"]")) return 7;
    if (!rejected(valid, "\"name\":\"Example\"", "\"name\":\"Example\",\"extra\":false")) return 8;
    if (!rejected(valid, "\"name\":\"Example\"", "\"name\":\"A\",\"name\":\"B\"")) return 9;
    if (!rejected(valid, "\"version\":\"1.0.0\"", "\"version\":\"01.0.0\"")) return 10;
    if (owo::plugin::parse_manifest(std::string("\xef\xbb\xbf") + valid).ok) return 11;
    if (owo::plugin::parse_manifest(valid + " garbage").ok) return 12;
    if (argc != 2) return 15;
    const auto loaded = owo::plugin::load_manifest(argv[1]);
    if (!loaded.ok || loaded.value.entry != "bin/example.exe") return 16;
    auto desktop_pet = valid;
    desktop_pet.replace(desktop_pet.find("\"permissions\":[]"),
        std::string("\"permissions\":[]").size(),
        "\"permissions\":[\"ui.desktop_pet\",\"system.full_trust\"]");
    if (!owo::plugin::parse_manifest(desktop_pet).ok) return 17;
    if (!rejected(desktop_pet, "\"system.full_trust\"", "\"clipboard.read\"")) return 18;
    auto network = valid;
    network.replace(network.find("\"permissions\":[]"),
        std::string("\"permissions\":[]").size(),
        "\"permissions\":[\"network.client\",\"system.full_trust\"]");
    network.replace(network.find("\"network\":false"),
        std::string("\"network\":false").size(), "\"network\":true");
    if (!owo::plugin::parse_manifest(network).ok) return 19;
    auto extension_api = valid;
    extension_api.replace(extension_api.find("\"permissions\":[]"),
        std::string("\"permissions\":[]").size(),
        "\"permissions\":[\"candidate.transform\",\"config.read\",\"ui.settings_page\"]");
    if (!owo::plugin::parse_manifest(extension_api).ok) return 20;
    auto model_resource = valid;
    model_resource.replace(model_resource.find("\"permissions\":[]"),
        std::string("\"permissions\":[]").size(),
        "\"permissions\":[\"resource.model.install\",\"system.full_trust\"]");
    if (!owo::plugin::parse_manifest(model_resource).ok ||
        !rejected(model_resource, ",\"system.full_trust\"", "")) return 21;
    return 0;
}
