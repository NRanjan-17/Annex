// Thin wrapper around CoreGraphics' private CGVirtualDisplay API.
//
// CGVirtualDisplay is undocumented. Everything that touches it lives in AnnexVirtualDisplay.m,
// which resolves the classes at run time (no link-time dependency), so a macOS update that
// removes or changes the API makes +isSupported return NO instead of crashing the app.
//
// Tested on: macOS 27.0.1 (Apple Silicon). See docs/TESTING.md for the tested-versions table.
// Behaviour notes (measured): mode sizes are in points; with hiDPI = YES the backing store is
// 2x (bounded by maxPixelsWide/High); the first mode is the default; macOS adds scaled and
// HiDPI variants on its own; releasing the object removes the display.

#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

@interface AnnexVirtualDisplayMode : NSObject
@property(nonatomic, readonly) uint32_t width;   // points
@property(nonatomic, readonly) uint32_t height;  // points
@property(nonatomic, readonly) double refreshRate;
+ (instancetype)modeWithWidth:(uint32_t)width height:(uint32_t)height refreshRate:(double)refreshRate;
@end

@interface AnnexVirtualDisplay : NSObject

/// YES if this macOS version provides the CGVirtualDisplay classes.
+ (BOOL)isSupported;

/// Creates the display (it appears in System Settings → Displays immediately).
/// vendorID/productID/serial give it a stable identity, so macOS remembers its arrangement,
/// resolution and scaling across reconnects.
- (nullable instancetype)initWithName:(NSString *)name
                             vendorID:(uint32_t)vendorID
                            productID:(uint32_t)productID
                               serial:(uint32_t)serial
                    sizeInMillimeters:(CGSize)sizeInMillimeters
                        maxPixelsWide:(uint32_t)maxPixelsWide
                        maxPixelsHigh:(uint32_t)maxPixelsHigh
                   terminationHandler:(nullable void (^)(void))terminationHandler;

/// Offers `modes` (first = default) to macOS. Returns NO if the system rejected them.
- (BOOL)applyModes:(NSArray<AnnexVirtualDisplayMode *> *)modes hiDPI:(BOOL)hiDPI;

@property(nonatomic, readonly) CGDirectDisplayID displayID;

/// Removes the display. Idempotent; also happens on dealloc.
- (void)invalidate;

@end

NS_ASSUME_NONNULL_END
