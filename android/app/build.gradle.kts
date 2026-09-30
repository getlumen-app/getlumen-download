plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "io.getlumen.app"
    compileSdk {
        version = release(36)
    }

    defaultConfig {
        applicationId = "io.getlumen.app"
        minSdk = 23
        targetSdk = 36
        versionCode = 1
        versionName = "1.0.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        create("release") {
            val storeFilePath = System.getenv("LUMEN_KEYSTORE_FILE")
            if (storeFilePath != null) {
                storeFile = file(storeFilePath)
                storePassword = System.getenv("LUMEN_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("LUMEN_KEY_ALIAS") ?: "lumen-android"
                keyPassword = System.getenv("LUMEN_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Release builds are signed with the dedicated keystore when the
            // LUMEN_KEYSTORE_* env vars are set; otherwise Gradle falls back
            // to the local debug keystore so smoke builds still install.
            val hasReleaseKey = System.getenv("LUMEN_KEYSTORE_FILE") != null
            if (hasReleaseKey) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    packagingOptions {
        // extractNativeLibs=true requires the libs uncompressed+aligned.
        jniLibs.useLegacyPackaging = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }
    kotlinOptions {
        jvmTarget = "11"
    }
}

dependencies {
    implementation(fileTree(mapOf("dir" to "libs", "include" to listOf("*.aar"))))
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.appcompat)
    implementation(libs.material)
    testImplementation(libs.junit)
    testImplementation(libs.orgjson)
}
