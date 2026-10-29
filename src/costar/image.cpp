#include "costar/image.hpp"

#include <dlfcn.h>
#include <stdlib.h>
#include <unistd.h>

#include <stdexcept>
#include <string>

namespace costar {

namespace {

// dlopen() hands back the already-loaded object for a file it has seen, so
// each Image loads its own copy of the file.
std::filesystem::path private_copy(const std::filesystem::path& file) {
  std::string path = (std::filesystem::temp_directory_path() / "costar-image-XXXXXX").string();
  const int fd = mkstemp(path.data());
  if (fd < 0) throw std::runtime_error("cannot create a temporary copy of " + file.string());
  close(fd);
  std::filesystem::copy_file(file, path, std::filesystem::copy_options::overwrite_existing);
  return path;
}

}  // namespace

Image::Image(const std::filesystem::path& file) : file_(file) {
  const std::filesystem::path copy = private_copy(file);
  handle_ = dlopen(copy.c_str(), RTLD_NOW | RTLD_LOCAL);
  std::filesystem::remove(copy);  // The mapping keeps the copy alive.
  if (handle_ == nullptr) throw std::runtime_error("cannot load " + file.string() + ": " + dlerror());
}

Image::~Image() {
  if (handle_ != nullptr) dlclose(handle_);
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
