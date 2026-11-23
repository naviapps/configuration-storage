// swift-tools-version: 6.3
import PackageDescription

let package = Package(
  name: "ConfigurationStorageConsumer",
  platforms: [.macOS(.v26)],
  dependencies: [.package(name: "configuration-storage", path: "../sdk")],
  targets: [
    .executableTarget(
      name: "ConfigurationStorageConsumer",
      dependencies: [
        .product(name: "ConfigurationStorage", package: "configuration-storage")
      ])
  ]
)
