#include "text_service.h"

#include <Windows.h>
#include <msctf.h>
#include <objbase.h>

#include <array>
#include <iterator>
#include <new>
#include <string>

namespace {
HMODULE module_handle = nullptr;

class ClassFactory final : public IClassFactory {
public:
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
        if (object == nullptr) return E_POINTER;
        *object = nullptr;
        if (iid == IID_IUnknown || iid == IID_IClassFactory) {
            *object = static_cast<IClassFactory*>(this);
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
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer,
                                             REFIID iid,
                                             void** object) override {
        if (outer != nullptr) return CLASS_E_NOAGGREGATION;
        return owo::tsf::create_text_service(iid, object);
    }
    HRESULT STDMETHODCALLTYPE LockServer(const BOOL lock) override {
        lock ? owo::tsf::increment_server_lock() : owo::tsf::decrement_server_lock();
        return S_OK;
    }

private:
    ~ClassFactory() = default;
    LONG references_{1};
};

std::wstring guid_string(const GUID& guid) {
    wchar_t buffer[40]{};
    return StringFromGUID2(guid, buffer, static_cast<int>(std::size(buffer))) > 0
               ? std::wstring(buffer)
               : std::wstring();
}

HRESULT register_com_server() {
    wchar_t module_path[MAX_PATH]{};
    if (GetModuleFileNameW(module_handle, module_path, MAX_PATH) == 0) {
        return HRESULT_FROM_WIN32(GetLastError());
    }
    const auto clsid = guid_string(owo::tsf::kTextServiceClsid);
    const std::wstring key_path = L"Software\\Classes\\CLSID\\" + clsid;
    HKEY class_key = nullptr;
    LSTATUS status = RegCreateKeyExW(HKEY_CURRENT_USER, key_path.c_str(), 0, nullptr, 0,
                                     KEY_WRITE, nullptr, &class_key, nullptr);
    if (status != ERROR_SUCCESS) return HRESULT_FROM_WIN32(status);
    const wchar_t description[] = L"OwO Input Method Text Service";
    status = RegSetValueExW(class_key, nullptr, 0, REG_SZ,
                            reinterpret_cast<const BYTE*>(description), sizeof(description));
    HKEY server_key = nullptr;
    if (status == ERROR_SUCCESS) {
        status = RegCreateKeyExW(class_key, L"InprocServer32", 0, nullptr, 0, KEY_WRITE,
                                 nullptr, &server_key, nullptr);
    }
    if (status == ERROR_SUCCESS) {
        status = RegSetValueExW(server_key, nullptr, 0, REG_SZ,
                                reinterpret_cast<const BYTE*>(module_path),
                                static_cast<DWORD>((wcslen(module_path) + 1) * sizeof(wchar_t)));
    }
    const wchar_t threading[] = L"Apartment";
    if (status == ERROR_SUCCESS) {
        status = RegSetValueExW(server_key, L"ThreadingModel", 0, REG_SZ,
                                reinterpret_cast<const BYTE*>(threading), sizeof(threading));
    }
    if (server_key != nullptr) RegCloseKey(server_key);
    RegCloseKey(class_key);
    return HRESULT_FROM_WIN32(status);
}

HRESULT unregister_com_server() {
    const std::wstring key_path = L"Software\\Classes\\CLSID\\" +
                                  guid_string(owo::tsf::kTextServiceClsid);
    const LSTATUS status = RegDeleteTreeW(HKEY_CURRENT_USER, key_path.c_str());
    return status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND
               ? S_OK
               : HRESULT_FROM_WIN32(status);
}

bool language_profile_exists(ITfInputProcessorProfiles* profiles) {
    IEnumTfLanguageProfiles* enumeration = nullptr;
    if (FAILED(profiles->EnumLanguageProfiles(owo::tsf::kSimplifiedChinese,
                                               &enumeration)) ||
        enumeration == nullptr) return false;
    bool found = false;
    TF_LANGUAGEPROFILE profile{};
    ULONG fetched = 0;
    while (enumeration->Next(1, &profile, &fetched) == S_OK) {
        if (profile.clsid == owo::tsf::kTextServiceClsid &&
            profile.guidProfile == owo::tsf::kLanguageProfileGuid) {
            found = true;
            break;
        }
    }
    enumeration->Release();
    return found;
}

HRESULT register_profile() {
    ITfInputProcessorProfiles* profiles = nullptr;
    HRESULT result = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
                                      CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&profiles));
    if (FAILED(result)) return result;
    const bool existing_profile = language_profile_exists(profiles);
    result = profiles->Register(owo::tsf::kTextServiceClsid);
    if (FAILED(result) && existing_profile) result = S_OK;
    if (SUCCEEDED(result) && !existing_profile) {
        const wchar_t description[] = L"OwO Input Method (P1 Prototype)";
        result = profiles->AddLanguageProfile(
            owo::tsf::kTextServiceClsid, owo::tsf::kSimplifiedChinese,
            owo::tsf::kLanguageProfileGuid, description,
            static_cast<ULONG>(wcslen(description)), L"", 0, 0);
        if (SUCCEEDED(result)) {
            // 注册不应改变用户当前输入法；测试宿主或设置界面必须显式启用。
            result = profiles->EnableLanguageProfile(
                owo::tsf::kTextServiceClsid, owo::tsf::kSimplifiedChinese,
                owo::tsf::kLanguageProfileGuid, FALSE);
        }
        if (FAILED(result)) profiles->Unregister(owo::tsf::kTextServiceClsid);
    }
    profiles->Release();
    return result;
}

bool category_exists(ITfCategoryMgr* categories, const GUID& category) {
    if (categories == nullptr) return false;
    IEnumGUID* enumeration = nullptr;
    if (FAILED(categories->EnumItemsInCategory(category, &enumeration)) ||
        enumeration == nullptr) return false;
    bool found = false;
    GUID item{};
    ULONG fetched = 0;
    while (enumeration->Next(1, &item, &fetched) == S_OK) {
        if (item == owo::tsf::kTextServiceClsid) {
            found = true;
            break;
        }
    }
    enumeration->Release();
    return found;
}

HRESULT register_categories() {
    ITfCategoryMgr* categories = nullptr;
    HRESULT result = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr,
                                      CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&categories));
    if (FAILED(result)) return result;
    const std::array capabilities{
        GUID_TFCAT_TIP_KEYBOARD,
        GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
        GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    };
    for (const auto& capability : capabilities) {
        const bool existing = category_exists(categories, capability);
        result = categories->RegisterCategory(owo::tsf::kTextServiceClsid,
                                              capability,
                                              owo::tsf::kTextServiceClsid);
        if (FAILED(result) && existing) result = S_OK;
        if (FAILED(result)) break;
    }
    categories->Release();
    return result;
}

void unregister_categories() {
    ITfCategoryMgr* categories = nullptr;
    if (SUCCEEDED(CoCreateInstance(CLSID_TF_CategoryMgr, nullptr,
                                   CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&categories)))) {
        const std::array capabilities{
            GUID_TFCAT_TIP_KEYBOARD,
            GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
            GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
        };
        for (const auto& capability : capabilities) {
            categories->UnregisterCategory(owo::tsf::kTextServiceClsid,
                                           capability,
                                           owo::tsf::kTextServiceClsid);
        }
        categories->Release();
    }
}

void unregister_profile() {
    ITfInputProcessorProfiles* profiles = nullptr;
    if (SUCCEEDED(CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
                                   CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&profiles)))) {
        profiles->RemoveLanguageProfile(owo::tsf::kTextServiceClsid,
                                        owo::tsf::kSimplifiedChinese,
                                        owo::tsf::kLanguageProfileGuid);
        profiles->Unregister(owo::tsf::kTextServiceClsid);
        profiles->Release();
    }
}
}  // namespace

BOOL WINAPI DllMain(const HINSTANCE instance, const DWORD reason, void*) {
    if (reason == DLL_PROCESS_ATTACH) {
        module_handle = instance;
        DisableThreadLibraryCalls(instance);
    }
    return TRUE;
}

STDAPI DllCanUnloadNow() {
    return owo::tsf::server_lock_count() == 0 ? S_OK : S_FALSE;
}

STDAPI DllGetClassObject(REFCLSID clsid, REFIID iid, void** object) {
    if (clsid != owo::tsf::kTextServiceClsid) return CLASS_E_CLASSNOTAVAILABLE;
    auto* factory = new (std::nothrow) ClassFactory();
    if (factory == nullptr) return E_OUTOFMEMORY;
    const HRESULT result = factory->QueryInterface(iid, object);
    factory->Release();
    return result;
}

extern "C" HRESULT __stdcall DllRegisterServer() {
    HRESULT result = register_com_server();
    if (FAILED(result)) return result;
    const HRESULT initialization = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    if (FAILED(initialization) && initialization != RPC_E_CHANGED_MODE) return initialization;
    result = register_profile();
    if (SUCCEEDED(result)) result = register_categories();
    if (SUCCEEDED(initialization)) CoUninitialize();
    if (FAILED(result)) {
        unregister_categories();
        unregister_profile();
        unregister_com_server();
    }
    return result;
}

extern "C" HRESULT __stdcall DllUnregisterServer() {
    const HRESULT initialization = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    unregister_profile();
    unregister_categories();
    const HRESULT registry_result = unregister_com_server();
    if (SUCCEEDED(initialization)) CoUninitialize();
    return registry_result;
}
