locals {
  drive_runtime_enabled = local.enabled && var.drive_runtime_image != ""
  drive_runtime_stage = (
    !local.enabled ? "disabled" :
    !local.drive_runtime_enabled ? "bootstrap" :
    var.drive_runtime_apply_mode ? "apply" : "dry-run"
  )
}

resource "terraform_data" "drive_runtime_guard" {
  count = var.enable_deployment ? 1 : 0

  lifecycle {
    precondition {
      condition = (
        !local.drive_runtime_enabled ||
        startswith(var.drive_runtime_image, "${var.region}-docker.pkg.dev/${var.project_id}/${var.artifact_repository}/")
      )
      error_message = "drive_runtime_image must come from the configured project, region, and Artifact Registry repository."
    }

    precondition {
      condition = (
        !local.drive_runtime_enabled ||
        (
          var.drive_runtime_operator != "" &&
          var.drive_boox_folder_id != "" &&
          var.drive_supernote_folder_id != "" &&
          var.drive_boox_folder_id != var.drive_supernote_folder_id
        )
      )
      error_message = "The Drive job requires an operator and two distinct exact Drive folder IDs."
    }

    precondition {
      condition     = var.drive_oauth_client_secret_id != var.drive_refresh_token_secret_id
      error_message = "The OAuth client and refresh token must use distinct Secret Manager containers."
    }

    precondition {
      condition = (
        !var.drive_runtime_apply_mode ||
        (
          local.drive_runtime_enabled &&
          var.drive_runtime_apply_acknowledgement == "I_UNDERSTAND_DRIVE_APPLY_MUTATES_SYNC_STATE"
        )
      )
      error_message = "Drive apply mode requires a deployed job and the exact documented acknowledgement."
    }
  }
}

# Terraform owns only the empty secret containers and their IAM policies.
# Secret versions are added out of band so OAuth bytes never enter plans or state.
resource "google_secret_manager_secret" "drive_oauth_client" {
  count = local.enabled ? 1 : 0

  project             = var.project_id
  secret_id           = var.drive_oauth_client_secret_id
  deletion_protection = true

  replication {
    auto {}
  }

  depends_on = [google_project_service.required]
}

resource "google_secret_manager_secret" "drive_refresh_token" {
  count = local.enabled ? 1 : 0

  project             = var.project_id
  secret_id           = var.drive_refresh_token_secret_id
  deletion_protection = true

  replication {
    auto {}
  }

  depends_on = [google_project_service.required]
}

resource "google_service_account" "drive_runtime" {
  count = local.enabled ? 1 : 0

  project      = var.project_id
  account_id   = "inkbridge-drive-gateway"
  display_name = "InkBridge Google Drive gateway job"

  depends_on = [google_project_service.required]
}

resource "google_storage_bucket_iam_member" "drive_runtime_objects" {
  count = local.enabled ? 1 : 0

  bucket = google_storage_bucket.sync[0].name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.drive_runtime[0].email}"
}

resource "google_project_iam_member" "drive_runtime_firestore" {
  count = local.enabled ? 1 : 0

  project = var.project_id
  role    = "roles/datastore.user"
  member  = "serviceAccount:${google_service_account.drive_runtime[0].email}"
}

resource "google_secret_manager_secret_iam_member" "drive_oauth_client_accessor" {
  count = local.enabled ? 1 : 0

  project   = var.project_id
  secret_id = google_secret_manager_secret.drive_oauth_client[0].secret_id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.drive_runtime[0].email}"
}

resource "google_secret_manager_secret_iam_member" "drive_refresh_token_accessor" {
  count = local.enabled ? 1 : 0

  project   = var.project_id
  secret_id = google_secret_manager_secret.drive_refresh_token[0].secret_id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.drive_runtime[0].email}"
}

resource "google_cloud_run_v2_job" "drive_runtime" {
  count = local.drive_runtime_enabled ? 1 : 0

  project             = var.project_id
  name                = "inkbridge-drive-gateway"
  location            = var.region
  deletion_protection = true
  labels = {
    component = "drive-gateway"
    mode      = var.drive_runtime_apply_mode ? "apply" : "dry-run"
  }

  template {
    task_count  = 1
    parallelism = 1

    template {
      service_account = google_service_account.drive_runtime[0].email
      timeout         = "900s"
      max_retries     = 0

      containers {
        image = var.drive_runtime_image
        args  = var.drive_runtime_apply_mode ? ["--apply"] : []

        resources {
          limits = {
            cpu    = "2"
            memory = "8Gi"
          }
        }

        env {
          name  = "INKBRIDGE_GCP_PROJECT"
          value = var.project_id
        }
        env {
          name  = "INKBRIDGE_GCS_BUCKET"
          value = google_storage_bucket.sync[0].name
        }
        env {
          name  = "INKBRIDGE_FIRESTORE_DATABASE"
          value = google_firestore_database.canonical[0].name
        }
        env {
          name  = "INKBRIDGE_DRIVE_BOOX_FOLDER_ID"
          value = var.drive_boox_folder_id
        }
        env {
          name  = "INKBRIDGE_DRIVE_SUPERNOTE_FOLDER_ID"
          value = var.drive_supernote_folder_id
        }
        env {
          name  = "INKBRIDGE_DRIVE_OAUTH_CLIENT_SECRET"
          value = google_secret_manager_secret.drive_oauth_client[0].secret_id
        }
        env {
          name  = "INKBRIDGE_DRIVE_REFRESH_TOKEN_SECRET"
          value = google_secret_manager_secret.drive_refresh_token[0].secret_id
        }
        env {
          name  = "INKBRIDGE_DRIVE_CHECKPOINT_ID"
          value = var.drive_checkpoint_id
        }
      }
    }
  }

  depends_on = [
    terraform_data.drive_runtime_guard,
    google_project_service.required,
    google_project_iam_member.drive_runtime_firestore,
    google_storage_bucket_iam_member.drive_runtime_objects,
    google_secret_manager_secret_iam_member.drive_oauth_client_accessor,
    google_secret_manager_secret_iam_member.drive_refresh_token_accessor,
  ]
}

resource "google_cloud_run_v2_job_iam_member" "drive_runtime_operator" {
  count = local.drive_runtime_enabled ? 1 : 0

  project  = var.project_id
  location = google_cloud_run_v2_job.drive_runtime[0].location
  name     = google_cloud_run_v2_job.drive_runtime[0].name
  role     = "roles/run.invoker"
  member   = var.drive_runtime_operator
}
