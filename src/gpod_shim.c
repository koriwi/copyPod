#include "gpod_shim.h"

#include <errno.h>
#include <stdarg.h>
#include <string.h>
#include <time.h>

#include <glib.h>
#include <glib/gstdio.h>
#include <gpod/itdb.h>

struct CpDb {
    Itdb_iTunesDB *itdb;
};

static void cp_set_error(char **out, const char *format, ...) {
    if (out == NULL) {
        return;
    }

    va_list args;
    va_start(args, format);
    *out = g_strdup_vprintf(format, args);
    va_end(args);
}

static void cp_set_gerror(char **out, const char *action, GError *error) {
    cp_set_error(out, "%s: %s", action,
                 error != NULL && error->message != NULL
                     ? error->message
                     : "unknown libgpod error");
    if (error != NULL) {
        g_error_free(error);
    }
}

CpDb *cp_db_open(const char *mountpoint, char **error) {
    GError *gerror = NULL;
    Itdb_iTunesDB *itdb = itdb_parse(mountpoint, &gerror);
    if (itdb == NULL) {
        cp_set_gerror(error, "could not read the iPod database", gerror);
        return NULL;
    }
    if (itdb_playlist_mpl(itdb) == NULL) {
        cp_set_error(error, "the iPod database has no master playlist");
        itdb_free(itdb);
        return NULL;
    }

    CpDb *db = g_new0(CpDb, 1);
    db->itdb = itdb;
    return db;
}

void cp_db_free(CpDb *db) {
    if (db == NULL) {
        return;
    }
    if (db->itdb != NULL) {
        itdb_free(db->itdb);
    }
    g_free(db);
}

char *cp_db_description(const CpDb *db) {
    if (db == NULL || db->itdb == NULL || db->itdb->device == NULL) {
        return g_strdup("unknown iPod");
    }

    const Itdb_IpodInfo *info = itdb_device_get_ipod_info(db->itdb->device);
    if (info == NULL) {
        return g_strdup("unknown iPod");
    }

    const char *generation =
        itdb_info_get_ipod_generation_string(info->ipod_generation);
    const char *model = itdb_info_get_ipod_model_name_string(info->ipod_model);
    return g_strdup_printf("%s%s%s", generation != NULL ? generation : "unknown iPod",
                           model != NULL ? " / " : "", model != NULL ? model : "");
}

int cp_db_requires_firewire_guid(const CpDb *db) {
    if (db == NULL || db->itdb == NULL || db->itdb->device == NULL) {
        return 0;
    }

    const Itdb_IpodInfo *info = itdb_device_get_ipod_info(db->itdb->device);
    if (info == NULL) {
        return 0;
    }

    switch (info->ipod_generation) {
        case ITDB_IPOD_GENERATION_CLASSIC_1:
        case ITDB_IPOD_GENERATION_CLASSIC_2:
        case ITDB_IPOD_GENERATION_CLASSIC_3:
        case ITDB_IPOD_GENERATION_NANO_3:
        case ITDB_IPOD_GENERATION_NANO_4:
        case ITDB_IPOD_GENERATION_NANO_5:
            return 1;
        default:
            return 0;
    }
}

char *cp_db_firewire_guid(const CpDb *db) {
    if (db == NULL || db->itdb == NULL || db->itdb->device == NULL) {
        return NULL;
    }

    /* libgpod copies FireWireGUID from SysInfoExtended into this SysInfo
       property while opening the device, so this covers both file formats. */
    return itdb_device_get_sysinfo(db->itdb->device, "FirewireGuid");
}

char *cp_db_database_path(const CpDb *db) {
    if (db == NULL || db->itdb == NULL) {
        return NULL;
    }
    return itdb_get_itunesdb_path(itdb_get_mountpoint(db->itdb));
}

size_t cp_db_track_count(const CpDb *db) {
    if (db == NULL || db->itdb == NULL) {
        return 0;
    }
    return (size_t)g_list_length(db->itdb->tracks);
}

CpTrackNode *cp_db_tracks(const CpDb *db) {
    return db != NULL && db->itdb != NULL ? (CpTrackNode *)db->itdb->tracks : NULL;
}

CpTrackNode *cp_track_node_next(const CpTrackNode *node) {
    return node != NULL ? (CpTrackNode *)((const GList *)node)->next : NULL;
}

CpTrack *cp_track_node_track(const CpTrackNode *node) {
    return node != NULL ? (CpTrack *)((const GList *)node)->data : NULL;
}

static char *cp_track_filesystem_path(Itdb_Track *track) {
    if (track == NULL) {
        return NULL;
    }
    if (track->itdb == NULL || track->ipod_path == NULL) {
        return itdb_filename_on_ipod(track);
    }

    /* Build the path directly instead of asking libgpod to resolve every path
       against the slow iPod filesystem. FAT paths are case-insensitive. */
    char *relative = g_strdup(track->ipod_path);
    itdb_filename_ipod2fs(relative);
    const char *mountpoint = itdb_get_mountpoint(track->itdb);
    char *path = relative[0] == '/'
                     ? g_strdup_printf("%s%s", mountpoint, relative)
                     : g_build_filename(mountpoint, relative, NULL);
    g_free(relative);
    return path;
}

char *cp_track_path(const CpTrack *opaque_track) {
    return cp_track_filesystem_path((Itdb_Track *)opaque_track);
}

char *cp_track_title(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return g_strdup(track != NULL && track->title != NULL ? track->title : "");
}

char *cp_track_album(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return g_strdup(track != NULL && track->album != NULL ? track->album : "");
}

char *cp_track_artist(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return g_strdup(track != NULL && track->artist != NULL ? track->artist : "");
}

char *cp_track_album_artist(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return g_strdup(track != NULL && track->albumartist != NULL ? track->albumartist : "");
}

uint64_t cp_track_size(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return track != NULL ? track->size : 0;
}

uint32_t cp_track_duration_ms(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return track != NULL && track->tracklen > 0 ? (uint32_t)track->tracklen : 0;
}

uint32_t cp_track_number(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return track != NULL && track->track_nr > 0 ? (uint32_t)track->track_nr : 0;
}

uint32_t cp_track_disc_number(const CpTrack *opaque_track) {
    const Itdb_Track *track = (const Itdb_Track *)opaque_track;
    return track != NULL && track->cd_nr > 0 ? (uint32_t)track->cd_nr : 0;
}

int cp_track_has_artwork(const CpTrack *opaque_track) {
    return opaque_track != NULL &&
           itdb_track_has_thumbnails((Itdb_Track *)opaque_track);
}

int cp_track_set_artwork(CpTrack *opaque_track, const unsigned char *data,
                         size_t data_len, char **error) {
    if (opaque_track == NULL || data == NULL || data_len == 0) {
        cp_set_error(error, "invalid track artwork request");
        return 0;
    }
    if (!itdb_track_set_thumbnails_from_data((Itdb_Track *)opaque_track,
                                              data, (gsize)data_len)) {
        cp_set_error(error, "libgpod could not decode the cover image");
        return 0;
    }
    return 1;
}

int cp_db_remove_track(CpDb *db, CpTrack *opaque_track, char **error) {
    if (db == NULL || db->itdb == NULL || opaque_track == NULL) {
        cp_set_error(error, "invalid track removal request");
        return 0;
    }

    Itdb_Track *track = (Itdb_Track *)opaque_track;
    char *path = cp_track_filesystem_path(track);
    if (path != NULL && g_unlink(path) != 0 && errno != ENOENT) {
        cp_set_error(error, "could not delete %s: %s", path, g_strerror(errno));
        g_free(path);
        return 0;
    }
    g_free(path);

    for (GList *item = db->itdb->playlists; item != NULL; item = item->next) {
        itdb_playlist_remove_track((Itdb_Playlist *)item->data, track);
    }
    if (itdb_track_has_thumbnails(track)) {
        itdb_track_remove_thumbnails(track);
    }
    itdb_track_remove(track);
    return 1;
}

static char *cp_dup(const char *value) {
    return g_strdup(value != NULL ? value : "");
}

int cp_db_add_track(CpDb *db, const char *source_path,
                    const CpMetadata *metadata, const unsigned char *artwork_data,
                    size_t artwork_len, char **copied_path, char **error) {
    if (db == NULL || db->itdb == NULL || source_path == NULL || metadata == NULL) {
        cp_set_error(error, "invalid track copy request");
        return 0;
    }

    Itdb_Track *track = itdb_track_new();
    if (track == NULL) {
        cp_set_error(error, "libgpod could not allocate a track");
        return 0;
    }

    track->title = cp_dup(metadata->title);
    track->album = cp_dup(metadata->album);
    track->artist = cp_dup(metadata->artist);
    track->albumartist = cp_dup(metadata->album_artist);
    track->genre = cp_dup(metadata->genre);
    track->comment = cp_dup(metadata->comment);
    track->filetype = g_strdup("MPEG audio file");
    track->size = (guint32)MIN(metadata->size, G_MAXUINT32);
    track->tracklen = (gint32)MIN(metadata->duration_ms, G_MAXINT32);
    track->bitrate = (gint32)MIN(metadata->bitrate_kbps, G_MAXINT32);
    track->samplerate = (guint16)MIN(metadata->sample_rate_hz, G_MAXUINT16);
    track->samplerate2 = (float)metadata->sample_rate_hz;
    track->year = (gint32)MIN(metadata->year, G_MAXINT32);
    track->track_nr = (gint32)MIN(metadata->track_number, G_MAXINT32);
    track->tracks = (gint32)MIN(metadata->track_total, G_MAXINT32);
    track->cd_nr = (gint32)MIN(metadata->disc_number, G_MAXINT32);
    track->cds = (gint32)MIN(metadata->disc_total, G_MAXINT32);
    track->time_added = time(NULL);
    track->time_modified = metadata->modified_at > 0
                               ? (time_t)metadata->modified_at
                               : track->time_added;
    track->mediatype = ITDB_MEDIATYPE_AUDIO;
    track->type2 = 0x01; /* MP3 */

    if (artwork_data != NULL && artwork_len > 0 &&
        !itdb_track_set_thumbnails_from_data(track, artwork_data,
                                              (gsize)artwork_len)) {
        cp_set_error(error, "libgpod could not decode the cover image");
        itdb_track_free(track);
        return 0;
    }

    Itdb_Playlist *master = itdb_playlist_mpl(db->itdb);
    itdb_track_add(db->itdb, track, -1);
    itdb_playlist_add_track(master, track, -1);

    GError *gerror = NULL;
    if (!itdb_cp_track_to_ipod(track, source_path, &gerror)) {
        itdb_playlist_remove_track(master, track);
        itdb_track_remove(track);
        cp_set_gerror(error, "could not copy track", gerror);
        return 0;
    }

    if (copied_path != NULL) {
        *copied_path = cp_track_filesystem_path(track);
    }
    return 1;
}

int cp_db_write(CpDb *db, char **error) {
    if (db == NULL || db->itdb == NULL) {
        cp_set_error(error, "invalid iPod database");
        return 0;
    }

    GError *gerror = NULL;
    if (!itdb_write(db->itdb, &gerror)) {
        cp_set_gerror(error, "could not write the iPod database", gerror);
        return 0;
    }
    return 1;
}

void cp_string_free(char *value) {
    g_free(value);
}
