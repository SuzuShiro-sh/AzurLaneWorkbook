# Native 第三方依赖

`AndKittyInjector/` 与其内部的 `KittyMemoryEx/` 本仓库直接管理，保留项目使用的编译开关、匿名映射名称、远程重映射与文件句柄管理实现。

| 依赖 | 上游来源 | 源码快照（含本项目修改） | 许可证 |
| --- | --- | --- | --- |
| AndKittyInjector | [MJx0/AndKittyInjector](https://github.com/MJx0/AndKittyInjector) | `27691f2ebe1d72404f6bc1734a1b244dc53dd390` | [MIT](AndKittyInjector/LICENSE) |
| KittyMemoryEx | [MJx0/KittyMemoryEx](https://github.com/MJx0/KittyMemoryEx) | `36db9245757b460b632abf0c63d80249a5c93b5e` | [MIT](AndKittyInjector/KittyMemoryEx/LICENSE) |

这两份依赖通过 `native/CMakeLists.txt` 从源码构建，不纳入上游附带的预编译注入测试库或 Keystone 静态库；本项目编译时关闭 Keystone。源码修改直接随本仓库提交。