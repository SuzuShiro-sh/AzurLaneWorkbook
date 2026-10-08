//! 启动只运行可取消任务的桌面窗口，不访问实际设备。
use azur_lane_workbook::interfaces::gui_controller::native_gui_fixture_factory;
use azur_lane_workbook::interfaces::native_gui::run_native_gui;
fn main() {
    if let Err(error) = run_native_gui(native_gui_fixture_factory()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
