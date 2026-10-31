#include "costar/image.hpp"

#include <dlfcn.h>
#include <stdlib.h>
#include <unistd.h>

#include <stdexcept>
#include <string>

namespace costar {

namespace {

#if defined(__SANITIZE_ADDRESS__)
#define COSTAR_ASAN 1
#elif defined(__has_feature)
#if __has_feature(address_sanitizer)
#define COSTAR_ASAN 1
#endif
#endif

// AddressSanitizer reads a module's symbols from its file when it reports,
// so in ASan builds the copy stays until the image is unloaded.  Otherwise
// it goes at once and can never be left behind.
#ifdef COSTAR_ASAN
constexpr bool kKeepCopy = true;
#else
constexpr bool kKeepCopy = false;
#endif

// dlopen() hands back the already-loaded object for a file it has seen, so
// each Image loads its own copy of the file: /tmp/costar-<name>-XXXXXX.so.
std::filesystem::path private_copy(const std::filesystem::path& file) {
  std::string path =
      (std::filesystem::temp_directory_path() / ("costar-" + file.stem().string() + "-XXXXXX.so")).string();
  const int fd = mkstemps(path.data(), 3);
  if (fd < 0) throw std::runtime_error("cannot create a temporary copy of " + file.string());
  close(fd);
  std::filesystem::copy_file(file, path, std::filesystem::copy_options::overwrite_existing);
  return path;
}

}  // namespace

Image::Image(const std::filesystem::path& file) : file_(file) {
  const std::filesystem::path copy = private_copy(file);
  handle_ = dlopen(copy.c_str(), RTLD_NOW | RTLD_LOCAL);
  if (handle_ != nullptr && kKeepCopy) {
    copy_ = copy;
  } else {
    std::filesystem::remove(copy);  // The mapping keeps the copy alive.
  }
  if (handle_ == nullptr) throw std::runtime_error("cannot load " + file.string() + ": " + dlerror());
}

Image::~Image() {
  if (handle_ != nullptr) dlclose(handle_);
  std::error_code error;
  if (!copy_.empty()) std::filesystem::remove(copy_, error);
}

void Image::attach(const costar_host& host, void* machine) {
  auto attach = reinterpret_cast<costar_image_attach_fn>(symbol(COSTAR_IMAGE_ATTACH_SYMBOL));
  if (attach(&host, machine) != 0) {
    throw std::runtime_error(file_.string() + " was built for a different costar ABI version");
  }
}

costar_image_reset_fn Image::reset() const {
  return reinterpret_cast<costar_image_reset_fn>(symbol(COSTAR_IMAGE_RESET_SYMBOL));
}

void* Image::symbol(const char* name) const {
  void* address = dlsym(handle_, name);
  if (address == nullptr) throw std::runtime_error(file_.string() + " does not export " + name);
  return address;
}

}  // namespace costar
