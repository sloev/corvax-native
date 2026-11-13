// Size budget: APK ≤ 6 MB. One small dependency (androidx.webkit); R8 shrinks the rest; split per ABI is not
// needed because there is no native code (WebRTC and crypto come from the system WebView).
plugins { id("com.android.application"); id("org.jetbrains.kotlin.android") }

android {
    namespace = "dev.corvax.node"
    compileSdk = 35
    defaultConfig { applicationId = "dev.corvax.node"; minSdk = 26; targetSdk = 35; versionCode = 1; versionName = "0.1.0" }
    buildTypes { release { isMinifyEnabled = true; isShrinkResources = true; proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt")) } }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    kotlinOptions { jvmTarget = "17" }
}

dependencies { implementation("androidx.webkit:webkit:1.12.1") }
