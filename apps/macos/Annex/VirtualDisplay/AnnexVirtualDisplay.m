#import "AnnexVirtualDisplay.h"

#import <objc/message.h>

// The only file in Annex that talks to the private CGVirtualDisplay classes. Properties are
// set with KVC and methods called through objc_msgSend so nothing is linked at build time.

static Class AVDClass(NSString *name) { return NSClassFromString(name); }

@implementation AnnexVirtualDisplayMode

+ (instancetype)modeWithWidth:(uint32_t)width height:(uint32_t)height refreshRate:(double)refreshRate {
    AnnexVirtualDisplayMode *m = [[self alloc] init];
    m->_width = width;
    m->_height = height;
    m->_refreshRate = refreshRate;
    return m;
}

@end

@implementation AnnexVirtualDisplay {
    id _display;  // CGVirtualDisplay *
}

+ (BOOL)isSupported {
    Class display = AVDClass(@"CGVirtualDisplay");
    return AVDClass(@"CGVirtualDisplayDescriptor") && AVDClass(@"CGVirtualDisplaySettings") &&
           AVDClass(@"CGVirtualDisplayMode") && display &&
           [display instancesRespondToSelector:NSSelectorFromString(@"initWithDescriptor:")] &&
           [display instancesRespondToSelector:NSSelectorFromString(@"applySettings:")] &&
           [display instancesRespondToSelector:NSSelectorFromString(@"displayID")];
}

- (nullable instancetype)initWithName:(NSString *)name
                             vendorID:(uint32_t)vendorID
                            productID:(uint32_t)productID
                               serial:(uint32_t)serial
                    sizeInMillimeters:(CGSize)sizeInMillimeters
                        maxPixelsWide:(uint32_t)maxPixelsWide
                        maxPixelsHigh:(uint32_t)maxPixelsHigh
                   terminationHandler:(nullable void (^)(void))terminationHandler {
    if (!(self = [super init]) || ![AnnexVirtualDisplay isSupported]) return nil;
    @try {
        id desc = [[AVDClass(@"CGVirtualDisplayDescriptor") alloc] init];
        [desc setValue:name forKey:@"name"];
        [desc setValue:@(vendorID) forKey:@"vendorID"];
        [desc setValue:@(productID) forKey:@"productID"];
        [desc setValue:@(serial) forKey:@"serialNum"];
        [desc setValue:@(maxPixelsWide) forKey:@"maxPixelsWide"];
        [desc setValue:@(maxPixelsHigh) forKey:@"maxPixelsHigh"];
        [desc setValue:[NSValue valueWithSize:NSSizeFromCGSize(sizeInMillimeters)] forKey:@"sizeInMillimeters"];
        [desc setValue:dispatch_get_main_queue() forKey:@"queue"];
        if (terminationHandler) {
            void (^handler)(id, id) = ^(id __unused a, id __unused b) { terminationHandler(); };
            [desc setValue:handler forKey:@"terminationHandler"];
        }
        id (*init)(id, SEL, id) = (id(*)(id, SEL, id))objc_msgSend;
        _display = init([AVDClass(@"CGVirtualDisplay") alloc], NSSelectorFromString(@"initWithDescriptor:"), desc);
    } @catch (NSException *e) {
        NSLog(@"Annex: CGVirtualDisplay creation failed: %@", e);
        _display = nil;
    }
    return _display ? self : nil;
}

- (BOOL)applyModes:(NSArray<AnnexVirtualDisplayMode *> *)modes hiDPI:(BOOL)hiDPI {
    if (!_display || modes.count == 0) return NO;
    @try {
        id settings = [[AVDClass(@"CGVirtualDisplaySettings") alloc] init];
        [settings setValue:@(hiDPI ? 1 : 0) forKey:@"hiDPI"];
        NSMutableArray *cgModes = [NSMutableArray arrayWithCapacity:modes.count];
        id (*initMode)(id, SEL, unsigned, unsigned, double) = (id(*)(id, SEL, unsigned, unsigned, double))objc_msgSend;
        for (AnnexVirtualDisplayMode *m in modes) {
            id mode = initMode([AVDClass(@"CGVirtualDisplayMode") alloc],
                               NSSelectorFromString(@"initWithWidth:height:refreshRate:"), m.width, m.height, m.refreshRate);
            if (mode) [cgModes addObject:mode];
        }
        [settings setValue:cgModes forKey:@"modes"];
        BOOL (*apply)(id, SEL, id) = (BOOL(*)(id, SEL, id))objc_msgSend;
        return apply(_display, NSSelectorFromString(@"applySettings:"), settings);
    } @catch (NSException *e) {
        NSLog(@"Annex: CGVirtualDisplay applySettings failed: %@", e);
        return NO;
    }
}

- (CGDirectDisplayID)displayID {
    if (!_display) return kCGNullDirectDisplay;
    CGDirectDisplayID (*getID)(id, SEL) = (CGDirectDisplayID(*)(id, SEL))objc_msgSend;
    return getID(_display, NSSelectorFromString(@"displayID"));
}

- (void)invalidate {
    _display = nil;  // releasing the CGVirtualDisplay removes the display
}

- (void)dealloc {
    _display = nil;
}

@end
