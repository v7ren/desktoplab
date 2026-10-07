//! Accessibility and Input Monitoring. Capture stays off until both are granted (R10).

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef)
        -> bool;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

const IOHID_REQUEST_LISTEN: u32 = 1;
const IOHID_ACCESS_GRANTED: u32 = 0;

pub fn accessibility() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn input_monitoring() -> bool {
    unsafe { IOHIDCheckAccess(IOHID_REQUEST_LISTEN) == IOHID_ACCESS_GRANTED }
}

pub fn prompt() {
    unsafe {
        let key = CFString::from_static_string("AXTrustedCheckOptionPrompt");
        let value = core_foundation::boolean::CFBoolean::true_value();
        let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
        AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef());
        let _ = IOHIDRequestAccess(IOHID_REQUEST_LISTEN);
    }
}
