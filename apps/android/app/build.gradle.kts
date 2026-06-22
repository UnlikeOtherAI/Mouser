import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "ai.unlikeother.mouser.companion"
    compileSdk = 34

    defaultConfig {
        // Bundle id parity with the iOS companion target.
        applicationId = "ai.unlikeother.mouser.companion"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        debug {
            isMinifyEnabled = false
        }
        release {
            // R8 on: shrink + obfuscate the release APK. We no longer pull
            // material-icons-extended (audit R2 LOW: release bloat), so the icon
            // set is small; R8 trims everything else unreachable too.
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    sourceSets {
        getByName("main") {
            java.srcDirs("src/main/kotlin")
        }
        getByName("test") {
            java.srcDirs("src/test/kotlin")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.09.02")
    implementation(composeBom)

    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.2")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.6")
    // Lifecycle-aware Compose effects (LifecycleEventEffect / LifecycleStartEffect)
    // used to pause the inertia/frame loop in the background and reconnect on resume
    // (audit R2 HIGH: app lifecycle). Pinned to the same lifecycle train as -ktx.
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.6")

    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    // Only the ~50-glyph core icon set (BOM-managed). The full
    // material-icons-extended artifact was dropped (audit R2 LOW: release bloat);
    // the handful of non-core glyphs we need are local vectors in MouserIcons.kt.
    implementation("androidx.compose.material:material-icons-core")

    // JNA is the runtime uniffi-Kotlin links the generated `mouser_ffi.kt` bindings
    // against (Native.register over libmouser_ffi.so, bundled per-ABI in jniLibs).
    // The `@aar` artifact is required on Android: it ships the JNA native dispatch
    // .so's inside the AAR so they're packaged into the APK.
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    debugImplementation("androidx.compose.ui:ui-tooling")

    testImplementation("junit:junit:4.13.2")
}
