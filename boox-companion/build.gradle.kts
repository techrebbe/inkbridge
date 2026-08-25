plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.inkbridge.boox"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.inkbridge.boox"
        minSdk = 30
        targetSdk = 34
        versionCode = 2
        versionName = "0.2.0-dev"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20250517")
}
