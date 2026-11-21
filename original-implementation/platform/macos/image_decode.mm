// platform/macos/image_decode.mm — ImageIO-backed decoding (see image_decode.hpp).
#include "image_decode.hpp"

#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>

namespace ed::mac {

vc::Image decode_image(const char* path) {
  vc::Image img;
  if (path == nullptr) return img;

  // A URL keeps the path out of CoreFoundation's hands, which matters for
  // anything with spaces or non-ASCII characters.
  NSString* ns = [NSString stringWithUTF8String:path];
  if (ns.length == 0) return img;
  NSURL* url = [NSURL fileURLWithPath:ns];
  CGImageSourceRef src =
      CGImageSourceCreateWithURL(reinterpret_cast<CFURLRef>(url), nullptr);
  if (src == nullptr) return img;

  // Decode the first frame at full size. kCGImageSourceShouldCacheImmediately
  // makes the pixels resident now, so the first draw is not a decode.
  CGImageRef cg = CGImageSourceCreateImageAtIndex(
      src, 0,
      (__bridge CFDictionaryRef)@{
        (__bridge NSString*)kCGImageSourceShouldCacheImmediately:
            (__bridge NSNumber*)kCFBooleanTrue,
      });
  if (cg != nullptr) {
    img.handle = cg;  // retained by Create
    img.width = static_cast<double>(CGImageGetWidth(cg));
    img.height = static_cast<double>(CGImageGetHeight(cg));
  }
  CFRelease(src);
  return img;
}

void release_image(vc::Image* img) {
  if (img == nullptr || img->handle == nullptr) return;
  CGImageRelease(static_cast<CGImageRef>(img->handle));
  img->handle = nullptr;
}

}  // namespace ed::mac