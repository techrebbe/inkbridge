locals {
  enabled = var.enable_deployment && var.deployment_acknowledgement == "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
  required_apis = toset([
    "billingbudgets.googleapis.com",
    "eventarc.googleapis.com",
    "firestore.googleapis.com",
    "pubsub.googleapis.com",
    "run.googleapis.com",
    "storage.googleapis.com",
  ])
}

resource "terraform_data" "deployment_guard" {
  count = var.enable_deployment ? 1 : 0

  lifecycle {
    precondition {
      condition     = !var.enable_deployment || local.enabled
      error_message = "Set deployment_acknowledgement exactly as documented before enabling billable resources."
    }
  }
}

resource "google_project_service" "required" {
  for_each = local.enabled ? local.required_apis : toset([])

  project            = var.project_id
  service            = each.value
  disable_on_destroy = false
}

resource "google_storage_bucket" "sync" {
  count = local.enabled ? 1 : 0

  name                        = var.bucket_name
  location                    = var.region
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"
  force_destroy               = false

  versioning {
    enabled = true
  }

  depends_on = [google_project_service.required]
}

resource "google_firestore_database" "canonical" {
  count = local.enabled ? 1 : 0

  project                           = var.project_id
  name                              = var.firestore_database
  location_id                       = var.region
  type                              = "FIRESTORE_NATIVE"
  delete_protection_state           = "DELETE_PROTECTION_ENABLED"
  deletion_policy                   = "ABANDON"
  point_in_time_recovery_enablement = "POINT_IN_TIME_RECOVERY_ENABLED"

  depends_on = [google_project_service.required]
}

resource "google_service_account" "runtime" {
  count = local.enabled ? 1 : 0

  project      = var.project_id
  account_id   = "inkbridge-runtime"
  display_name = "InkBridge Cloud Run broker"
}

resource "google_service_account" "eventarc" {
  count = local.enabled ? 1 : 0

  project      = var.project_id
  account_id   = "inkbridge-eventarc"
  display_name = "InkBridge Eventarc invoker"
}

resource "google_storage_bucket_iam_member" "runtime_objects" {
  count = local.enabled ? 1 : 0

  bucket = google_storage_bucket.sync[0].name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.runtime[0].email}"
}

resource "google_project_iam_member" "runtime_firestore" {
  count = local.enabled ? 1 : 0

  project = var.project_id
  role    = "roles/datastore.user"
  member  = "serviceAccount:${google_service_account.runtime[0].email}"
}

resource "google_project_iam_member" "eventarc_receiver" {
  count = local.enabled ? 1 : 0

  project = var.project_id
  role    = "roles/eventarc.eventReceiver"
  member  = "serviceAccount:${google_service_account.eventarc[0].email}"
}

resource "google_project_iam_member" "storage_pubsub" {
  count = local.enabled ? 1 : 0

  project = var.project_id
  role    = "roles/pubsub.publisher"
  member  = "serviceAccount:service-${var.project_number}@gs-project-accounts.iam.gserviceaccount.com"
}

resource "google_cloud_run_v2_service" "runtime" {
  count = local.enabled ? 1 : 0

  project             = var.project_id
  name                = "inkbridge-broker"
  location            = var.region
  ingress             = "INGRESS_TRAFFIC_INTERNAL_ONLY"
  deletion_protection = true

  template {
    service_account                  = google_service_account.runtime[0].email
    timeout                          = "300s"
    max_instance_request_concurrency = 1

    scaling {
      min_instance_count = 0
      max_instance_count = 1
    }

    containers {
      image = var.cloud_run_image

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
    }
  }

  depends_on = [
    google_project_service.required,
    google_project_iam_member.runtime_firestore,
    google_storage_bucket_iam_member.runtime_objects,
  ]
}

resource "google_cloud_run_v2_service_iam_member" "eventarc_invoker" {
  count = local.enabled ? 1 : 0

  project  = var.project_id
  location = google_cloud_run_v2_service.runtime[0].location
  name     = google_cloud_run_v2_service.runtime[0].name
  role     = "roles/run.invoker"
  member   = "serviceAccount:${google_service_account.eventarc[0].email}"
}

resource "google_eventarc_trigger" "storage_finalized" {
  count = local.enabled ? 1 : 0

  project         = var.project_id
  name            = "inkbridge-storage-finalized"
  location        = var.region
  service_account = google_service_account.eventarc[0].email

  matching_criteria {
    attribute = "type"
    value     = "google.cloud.storage.object.v1.finalized"
  }
  matching_criteria {
    attribute = "bucket"
    value     = google_storage_bucket.sync[0].name
  }

  destination {
    cloud_run_service {
      service = google_cloud_run_v2_service.runtime[0].name
      region  = google_cloud_run_v2_service.runtime[0].location
      path    = "/"
    }
  }

  depends_on = [
    google_cloud_run_v2_service_iam_member.eventarc_invoker,
    google_project_iam_member.eventarc_receiver,
    google_project_iam_member.storage_pubsub,
  ]
}

resource "google_billing_budget" "inkbridge" {
  count = local.enabled && var.monthly_budget_usd > 0 ? 1 : 0

  billing_account = var.billing_account
  display_name    = "InkBridge monthly budget"

  budget_filter {
    projects = ["projects/${var.project_number}"]
  }

  amount {
    specified_amount {
      currency_code = "USD"
      units         = tostring(floor(var.monthly_budget_usd))
      nanos         = floor((var.monthly_budget_usd - floor(var.monthly_budget_usd)) * 1000000000)
    }
  }

  threshold_rules {
    threshold_percent = 0.5
  }
  threshold_rules {
    threshold_percent = 0.9
  }
  threshold_rules {
    threshold_percent = 1.0
  }
}
