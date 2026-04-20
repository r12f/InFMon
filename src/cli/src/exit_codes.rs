/// Exit codes for infmonctl (frozen v1)
pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_NOT_FOUND: i32 = 3;
pub const EXIT_FRONTEND_UNREACHABLE: i32 = 4;
pub const EXIT_BACKEND_NOT_READY: i32 = 5;
pub const EXIT_CONFLICT: i32 = 6;
pub const EXIT_DEGRADED: i32 = 7;
pub const EXIT_PERMISSION_DENIED: i32 = 13;
pub const EXIT_SERVICE_UNAVAILABLE: i32 = 69;
pub const EXIT_SIGINT: i32 = 130;
pub const EXIT_SIGTERM: i32 = 143;
